//! `send`: stage files/dirs/clipboard into the outbox.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::herdr::{self, Ctx};
use crate::state::{Item, Kind, State, OUTBOX_CAP_BYTES};
use crate::util::{self, sha256_bytes, sha256_file};

pub struct SendOpts {
    pub note: Option<String>,
    pub clip: bool,
    pub force: bool,
    pub allow_outside_home: bool,
}

pub fn send(state: &State, ctx: &Ctx, paths: &[PathBuf], opts: &SendOpts) -> Result<Vec<String>> {
    if opts.clip {
        let id = send_clipboard(state, ctx, opts)?;
        herdr::report_all_tokens(state, ctx);
        return Ok(vec![id]);
    }
    if paths.is_empty() {
        bail!("nothing to send: give a path or --clip");
    }
    let base = ctx.pane_cwd();
    let home = util::home_dir();
    let mut ids = Vec::new();
    for raw in paths {
        let p = util::expand_tilde(raw);
        let full = if p.is_absolute() { p } else { base.join(p) };
        let full = full.canonicalize().with_context(|| format!("{} does not exist", full.display()))?;
        if !opts.allow_outside_home && !full.starts_with(&home) {
            bail!("{} is outside $HOME; pass --allow-outside-home", full.display());
        }
        let name = full.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "item".into());
        if let Some(rule) = util::denied_reason(&name) {
            if !opts.force {
                bail!("refusing to send {name}: matches secrets deny-list ({rule}); pass --force to override");
            }
            tracing::warn!("sending {name} despite deny-list rule {rule} (--force)");
        }
        let id = if full.is_dir() { stage_dir(state, ctx, &full, &name, opts)? } else { stage_file(state, ctx, &full, &name, opts)? };
        ids.push(id);
    }
    herdr::report_all_tokens(state, ctx);
    Ok(ids)
}

fn check_cap(state: &State, add: u64) -> Result<()> {
    let used = state.outbox_bytes()?;
    if used + add > OUTBOX_CAP_BYTES {
        bail!(
            "outbox cap exceeded: {} queued + {} new > {}; run `herdr-ferry pull` on the laptop first",
            util::human_size(used),
            util::human_size(add),
            util::human_size(OUTBOX_CAP_BYTES)
        );
    }
    Ok(())
}

fn finish(state: &State, ctx: &Ctx, mut item: Item, write_payload: impl FnOnce(&Path) -> Result<()>) -> Result<String> {
    if let Some(existing) = state.find_dedupe(&item.sha256)? {
        tracing::info!("dedupe: {} already queued as {}", item.name, existing.id);
        return Ok(existing.id);
    }
    check_cap(state, item.size)?;
    item.source = ctx.source();
    let dir = state.item_dir(true, &item.id);
    fs::create_dir_all(&dir)?;
    let payload = state.outbox_payload(&item);
    if let Err(e) = write_payload(&payload) {
        let _ = fs::remove_dir_all(&dir);
        return Err(e);
    }
    state.write_item(true, &item)?;
    tracing::info!("queued {} {} ({})", item.id, item.name, util::human_size(item.size));
    Ok(item.id)
}

fn stage_file(state: &State, ctx: &Ctx, full: &Path, name: &str, opts: &SendOpts) -> Result<String> {
    let size = util::file_size(full)?;
    let sha = sha256_file(full)?;
    let item = Item::new(Kind::File, name, size, &sha, &util::mime_for(name), opts.note.clone(), Default::default());
    finish(state, ctx, item, |payload| {
        fs::copy(full, payload).with_context(|| format!("copy {}", full.display()))?;
        Ok(())
    })
}

fn stage_dir(state: &State, ctx: &Ctx, full: &Path, name: &str, opts: &SendOpts) -> Result<String> {
    // Tar into a temp file first so we can hash + size it before creating the item.
    let tmp = state.outbox_dir().join(format!(".staging-{}.tar.gz", ulid::Ulid::new()));
    {
        let f = File::create(&tmp)?;
        let enc = GzEncoder::new(f, Compression::fast());
        let mut tar = tar::Builder::new(enc);
        tar.follow_symlinks(false);
        tar.append_dir_all(name, full).with_context(|| format!("tar {}", full.display()))?;
        tar.into_inner()?.finish()?;
    }
    let size = util::file_size(&tmp)?;
    let sha = sha256_file(&tmp)?;
    let item = Item::new(Kind::Dir, name, size, &sha, "application/gzip", opts.note.clone(), Default::default());
    let res = finish(state, ctx, item, |payload| {
        fs::rename(&tmp, payload)?;
        Ok(())
    });
    let _ = fs::remove_file(&tmp);
    res
}

fn send_clipboard(state: &State, ctx: &Ctx, opts: &SendOpts) -> Result<String> {
    if let Some(png) = read_clipboard_image()? {
        let sha = sha256_bytes(&png);
        let name = format!("clipboard-{}.png", chrono_stamp());
        let item = Item::new(Kind::ClipboardImage, &name, png.len() as u64, &sha, "image/png", opts.note.clone(), Default::default());
        return finish(state, ctx, item, |payload| Ok(fs::write(payload, &png)?));
    }
    let text = read_clipboard_text()?;
    if text.is_empty() {
        bail!("clipboard is empty (no image, no text)");
    }
    let sha = sha256_bytes(text.as_bytes());
    let name = format!("clipboard-{}.txt", chrono_stamp());
    let item = Item::new(Kind::ClipboardText, &name, text.len() as u64, &sha, "text/plain", opts.note.clone(), Default::default());
    finish(state, ctx, item, |payload| Ok(fs::write(payload, text.as_bytes())?))
}

fn chrono_stamp() -> String {
    // seconds since epoch is unique enough for a filename and needs no chrono dep
    (util::now_ms() / 1000).to_string()
}

fn read_clipboard_image() -> Result<Option<Vec<u8>>> {
    if util::os_is_macos() {
        let tmp = std::env::temp_dir().join(format!("herdr-ferry-clip-{}.png", std::process::id()));
        let script = format!(
            "set f to open for access POSIX file \"{}\" with write permission\ntry\nwrite (the clipboard as «class PNGf») to f\nend try\nclose access f",
            tmp.display()
        );
        let _ = Command::new("osascript").arg("-e").arg(&script).output();
        let bytes = fs::read(&tmp).unwrap_or_default();
        let _ = fs::remove_file(&tmp);
        return Ok(if bytes.is_empty() { None } else { Some(bytes) });
    }
    if util::which("wl-paste") {
        if let Ok(types) = util::run("wl-paste", &["-l"]) {
            if types.contains("image/png") {
                let out = Command::new("wl-paste").args(["-t", "image/png"]).output()?;
                if out.status.success() && !out.stdout.is_empty() {
                    return Ok(Some(out.stdout));
                }
            }
        }
    }
    if util::which("xclip") {
        let out = Command::new("xclip").args(["-selection", "clipboard", "-t", "image/png", "-o"]).output()?;
        if out.status.success() && !out.stdout.is_empty() {
            return Ok(Some(out.stdout));
        }
    }
    Ok(None)
}

fn read_clipboard_text() -> Result<String> {
    if util::os_is_macos() {
        return util::run("pbpaste", &[]);
    }
    if util::which("wl-paste") {
        return util::run("wl-paste", &["-n"]);
    }
    if util::which("xclip") {
        return util::run("xclip", &["-selection", "clipboard", "-o"]);
    }
    bail!("no clipboard tool found (pbpaste / wl-paste / xclip)")
}

/// Plugin action entry point: selected text that is a path → send it; else open the picker.
pub fn send_from_context(state: &State, ctx: &Ctx) -> Result<u8> {
    let base = ctx.pane_cwd();
    // A clicked file:// URL (link handler) counts as a selection.
    let candidate = ctx
        .context
        .clicked_url
        .clone()
        .or_else(|| std::env::var("HERDR_PLUGIN_CLICKED_URL").ok())
        .map(|u| u.trim_start_matches("file://").to_string())
        .or_else(|| ctx.selected_text());
    if let Some(sel) = candidate {
        let first = sel.lines().next().unwrap_or("").trim().trim_matches(|c| c == '"' || c == '\'' || c == '`');
        if let Some(p) = herdr::path_exists_rel(&base, first) {
            let opts = SendOpts { note: None, clip: false, force: false, allow_outside_home: false };
            let ids = send(state, ctx, &[p.clone()], &opts)?;
            println!("queued {} as {}", p.display(), ids.join(","));
            return Ok(0);
        }
    }
    ctx.open_picker()?;
    Ok(0)
}
