//! Laptop side: watch / pull / push, clipboard, notifications.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::ssh::{load_config, Remote};
use crate::state::{Item, Kind};
use crate::util::{self, sha256_file};

struct Client {
    remote: Remote,
    dest: PathBuf,
    open: bool,
}

fn default_dest() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| util::home_dir().join("Downloads"))
        .join("herdr-ferry")
}

impl Client {
    fn new(alias: Option<String>, dest: Option<PathBuf>, open: bool) -> Result<Client> {
        let cfg = load_config();
        let remote = Remote::resolve(alias)?;
        let dest = dest.or(cfg.dest).map(|d| util::expand_tilde(&d)).unwrap_or_else(default_dest);
        fs::create_dir_all(&dest).with_context(|| format!("create {}", dest.display()))?;
        Ok(Client { remote, dest, open })
    }

    fn check_version(&self, header: &str) {
        let Some(remote) = header.strip_prefix("#herdr-ferry ") else { return };
        let remote = remote.trim();
        if remote == crate::VERSION {
            return;
        }
        let major = |v: &str| v.split('.').next().unwrap_or("0").to_string();
        if major(remote) != major(crate::VERSION) {
            eprintln!("herdr-ferry: remote is {remote}, local is {} — major mismatch, refusing", crate::VERSION);
            std::process::exit(1);
        }
        eprintln!("herdr-ferry: warning: remote is {remote}, local is {}", crate::VERSION);
    }

    /// Drain every queued item once. Returns number delivered.
    fn drain(&self) -> Result<usize> {
        let out = self.remote.run_ok(&["outbox-list", "--json"])?;
        let items: Vec<Item> = serde_json::from_str(out.trim()).context("parse outbox-list")?;
        let mut n = 0;
        for it in items.into_iter().filter(|i| matches!(i.state, crate::state::ItemState::Queued)) {
            match self.deliver(&it) {
                Ok(()) => n += 1,
                Err(e) => eprintln!("herdr-ferry: {} ({}): {e:#}", it.name, it.id),
            }
        }
        Ok(n)
    }

    fn deliver(&self, it: &Item) -> Result<()> {
        self.remote.run_ok(&["outbox-claim", &it.id])?;
        let tmp = self.dest.join(format!(".{}.part", it.id));
        let mut ok = false;
        for attempt in 1..=2 {
            {
                let mut w = BufWriter::new(File::create(&tmp)?);
                self.remote.run_to_writer(&["outbox-cat", &it.id], &mut w)?;
            }
            let got = sha256_file(&tmp)?;
            if got == it.sha256 {
                ok = true;
                break;
            }
            tracing::warn!("sha256 mismatch for {} (attempt {attempt})", it.id);
        }
        if !ok {
            let _ = fs::remove_file(&tmp);
            bail!("sha256 mismatch twice; left claimed on server");
        }
        self.remote.run_ok(&["outbox-ack", &it.id])?;

        let body = it.note.clone().unwrap_or_else(|| format!("{} ({})", it.name, util::human_size(it.size)));
        let mut saved: Option<PathBuf> = None;
        match it.kind {
            Kind::ClipboardText => {
                let text = fs::read_to_string(&tmp)?;
                fs::remove_file(&tmp)?;
                set_clipboard_text(&text)?;
                notify("Ferry: clipboard text", &body, None);
            }
            Kind::Dir => {
                let out_dir = util::unique_dest(&self.dest, &it.name);
                fs::create_dir_all(&out_dir)?;
                let f = File::open(&tmp)?;
                let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
                // archive entries are prefixed with <name>/ — strip it so we land in out_dir directly
                for entry in ar.entries()? {
                    let mut entry = entry?;
                    let path = entry.path()?.into_owned();
                    let rel: PathBuf = path.components().skip(1).collect();
                    if rel.as_os_str().is_empty() {
                        continue;
                    }
                    entry.unpack(out_dir.join(rel))?;
                }
                fs::remove_file(&tmp)?;
                notify("Ferry: directory", &format!("{} → {}", body, out_dir.display()), Some(&out_dir));
                saved = Some(out_dir);
            }
            Kind::File | Kind::ClipboardImage => {
                let dest = util::unique_dest(&self.dest, &it.name);
                fs::rename(&tmp, &dest)?;
                if util::is_image_mime(&it.mime) {
                    if let Err(e) = set_clipboard_image(&dest, &it.mime) {
                        tracing::warn!("clipboard: {e:#}");
                    }
                    notify("Ferry: image (on clipboard)", &body, Some(&dest));
                } else {
                    notify("Ferry: file", &body, Some(&dest));
                }
                saved = Some(dest);
            }
        }
        if let (true, Some(p)) = (self.open, &saved) {
            open_path(p);
        }
        println!("{}\t{}\t{}", it.id, it.name, saved.map(|p| p.display().to_string()).unwrap_or_else(|| "(clipboard)".into()));
        Ok(())
    }
}

pub fn pull(alias: Option<String>, dest: Option<PathBuf>, open: bool) -> Result<u8> {
    let c = Client::new(alias, dest, open)?;
    let n = c.drain()?;
    if n == 0 {
        println!("nothing queued");
    }
    Ok(0)
}

pub fn watch(alias: Option<String>, dest: Option<PathBuf>, open: bool) -> Result<u8> {
    let c = Client::new(alias, dest, open)?;
    eprintln!("herdr-ferry: watching {} → {}", c.remote.alias, c.dest.display());
    let mut backoff = Duration::from_secs(1);
    loop {
        match c.remote.run(&["outbox-wait", "--timeout", "55"]) {
            Ok((code, out)) if code == 0 || code == 2 => {
                backoff = Duration::from_secs(1);
                if let Some(h) = out.lines().next() {
                    c.check_version(h);
                }
                if code == 0 {
                    if let Err(e) = c.drain() {
                        eprintln!("herdr-ferry: drain: {e:#}");
                    }
                }
            }
            Ok((code, _)) => {
                eprintln!("herdr-ferry: ssh/remote exited {code}; retrying in {}s", backoff.as_secs());
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
            Err(e) => {
                eprintln!("herdr-ferry: {e:#}; retrying in {}s", backoff.as_secs());
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

pub fn push(alias: Option<String>, paths: &[PathBuf], paste: bool, note: Option<String>) -> Result<u8> {
    if paths.is_empty() {
        bail!("nothing to push");
    }
    let remote = Remote::resolve(alias)?;
    let mut last_id = None;
    for raw in paths {
        let p = util::expand_tilde(raw).canonicalize().with_context(|| format!("{} does not exist", raw.display()))?;
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).context("bad path")?;
        let (file, send_name, kind, cleanup): (PathBuf, String, &str, Option<PathBuf>) = if p.is_dir() {
            let tmp = std::env::temp_dir().join(format!("herdr-ferry-push-{}.tar.gz", std::process::id()));
            let f = File::create(&tmp)?;
            let mut tar = tar::Builder::new(GzEncoder::new(f, Compression::fast()));
            tar.follow_symlinks(false);
            tar.append_dir_all(&name, &p)?;
            tar.into_inner()?.finish()?;
            (tmp.clone(), format!("{name}.tar.gz"), "dir", Some(tmp))
        } else {
            (p.clone(), name.clone(), "file", None)
        };
        let size = util::file_size(&file)?.to_string();
        let sha = sha256_file(&file)?;
        let mime = util::mime_for(&send_name);
        let mut args = vec!["inbox-receive", "--name", &send_name, "--sha256", &sha, "--size", &size, "--kind", kind, "--mime", &mime];
        if let Some(n) = &note {
            args.extend(["--note", n]);
        }
        let id = remote.run_from_file(&args, &file);
        if let Some(t) = cleanup {
            let _ = fs::remove_file(t);
        }
        let id = id?;
        println!("{id}\t{name}");
        last_id = Some(id);
    }
    if paste {
        if let Some(id) = last_id {
            let out = remote.run_ok(&["inbox-paste", &id])?;
            print!("{out}");
        }
    }
    Ok(0)
}

// ---- OS integration ----

pub fn set_clipboard_text(text: &str) -> Result<()> {
    let tool: (&str, Vec<&str>) = if util::os_is_macos() {
        ("pbcopy", vec![])
    } else if util::which("wl-copy") {
        ("wl-copy", vec![])
    } else if util::which("xclip") {
        ("xclip", vec!["-selection", "clipboard"])
    } else {
        bail!("no clipboard tool (pbcopy / wl-copy / xclip)")
    };
    let mut child = Command::new(tool.0).args(&tool.1).stdin(std::process::Stdio::piped()).spawn()?;
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), text.as_bytes())?;
    child.wait()?;
    Ok(())
}

pub fn set_clipboard_image(path: &Path, mime: &str) -> Result<()> {
    if util::os_is_macos() {
        let class = match mime {
            "image/png" => "«class PNGf»",
            "image/jpeg" => "JPEG picture",
            "image/gif" => "GIF picture",
            "image/tiff" => "TIFF picture",
            _ => bail!("{mime} not supported on macOS clipboard"),
        };
        let script = format!("set the clipboard to (read (POSIX file \"{}\") as {class})", path.display());
        util::run("osascript", &["-e", &script])?;
        return Ok(());
    }
    let p = path.to_string_lossy().to_string();
    if util::which("wl-copy") {
        util::run("sh", &["-c", &format!("wl-copy -t {} < {}", util::sh_quote(mime), util::sh_quote(&p))])?;
    } else if util::which("xclip") {
        util::run("xclip", &["-selection", "clipboard", "-t", mime, "-i", &p])?;
    } else {
        bail!("no clipboard tool (wl-copy / xclip)");
    }
    Ok(())
}

pub fn notify(title: &str, body: &str, path: Option<&Path>) {
    if util::os_is_macos() {
        if util::which("terminal-notifier") {
            let mut args = vec!["-title".to_string(), title.into(), "-message".into(), body.into(), "-group".into(), "herdr-ferry".into()];
            if let Some(p) = path {
                args.extend(["-open".into(), format!("file://{}", p.display())]);
            }
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            if util::run_ok("terminal-notifier", &a) {
                return;
            }
        }
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title));
        let _ = util::run("osascript", &["-e", &script]);
    } else if util::which("notify-send") {
        let _ = util::run("notify-send", &["-a", "herdr-ferry", title, body]);
    }
}

pub fn open_path(p: &Path) {
    let bin = if util::os_is_macos() { "open" } else { "xdg-open" };
    let _ = Command::new(bin).arg(p).spawn();
}
