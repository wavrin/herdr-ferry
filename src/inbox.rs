use std::fs::{self, File};
use std::io::{self, BufWriter};

use anyhow::{bail, Context, Result};

use crate::herdr::Ctx;
use crate::state::{print_items, Item, ItemState, Kind, State};
use crate::util::{self, sha256_file};

pub fn list(state: &State, json: bool) -> Result<()> {
    print_items(&state.list_inbox()?, json)
}

/// Receive a payload on stdin (used by `push` over ssh). Never overwrites: fresh ULID dir.
pub fn receive(state: &State, name: &str, sha256: &str, size: u64, kind: &str, mime: Option<String>, note: Option<String>) -> Result<String> {
    let name = std::path::Path::new(name).file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| !n.is_empty()).context("bad name")?;
    let kind = Kind::parse(kind)?;
    let mime = mime.unwrap_or_else(|| util::mime_for(&name));
    let mut item = Item::new(kind, &name, size, sha256, &mime, note, Default::default());
    item.state = ItemState::Acked;
    let dir = state.item_dir(false, &item.id);
    fs::create_dir_all(&dir)?;
    let dest = dir.join(&name);
    {
        let mut w = BufWriter::new(File::create(&dest)?);
        let n = io::copy(&mut io::stdin().lock(), &mut w)?;
        if n != size {
            let _ = fs::remove_dir_all(&dir);
            bail!("short read: expected {size} bytes, got {n}");
        }
    }
    let got = sha256_file(&dest)?;
    if got != sha256 {
        let _ = fs::remove_dir_all(&dir);
        bail!("sha256 mismatch on receive");
    }
    if kind == Kind::Dir {
        // arrives as <name>.tar.gz; extract beside it so the pasted path is a real directory
        let out = dir.join(name.trim_end_matches(".tar.gz"));
        let f = File::open(&dest)?;
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
        ar.unpack(&dir)?;
        let _ = fs::remove_file(&dest);
        if out.exists() {
            item.name = out.file_name().unwrap().to_string_lossy().to_string();
        }
    }
    state.write_item(false, &item)?;
    state.append_history(&item, "inbox")?;
    tracing::info!("inbox {} {}", item.id, item.name);
    Ok(item.id)
}

/// Insert the item's path into the focused pane. Text only — never Enter.
pub fn paste(state: &State, ctx: &Ctx, id: Option<&str>) -> Result<()> {
    let item = match id {
        Some(id) => state.read_item(false, id)?,
        None => state.list_inbox()?.into_iter().last().context("inbox is empty")?,
    };
    let path = state.inbox_payload(&item);
    let pane = ctx.resolve_pane()?;
    let text = path.display().to_string();
    ctx.send_text(&pane, &text)?;
    println!("pasted {text} into {pane}");
    Ok(())
}
