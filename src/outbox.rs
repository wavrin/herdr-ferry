use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};

use crate::herdr::{self, Ctx};
use crate::state::{print_items, State};

pub fn list(state: &State, json: bool) -> Result<()> {
    print_items(&state.list_outbox()?, json)
}

/// Prints a version header, then `id\tname\tsize\tkind` lines. Exit 0 = items, 2 = timeout.
pub fn wait(state: &State, timeout_secs: u64) -> Result<u8> {
    println!("#herdr-ferry {}", crate::VERSION);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    })?;
    watcher.watch(&state.outbox_dir(), RecursiveMode::Recursive).context("watch outbox")?;
    loop {
        let queued = state.queued_outbox()?;
        if !queued.is_empty() {
            for it in queued {
                println!("{}\t{}\t{}\t{}", it.id, it.name, it.size, it.kind.as_str());
            }
            return Ok(0);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(2);
        }
        // fs events wake us early; the 5s cap is a safety net for missed events
        let _ = rx.recv_timeout(left.min(Duration::from_secs(5)));
        // debounce: sidecar is written after payload, give it a moment
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn claim(state: &State, id: &str) -> Result<()> {
    let it = state.claim(id)?;
    println!("{}\t{}\t{}\t{}\t{}", it.id, it.name, it.size, it.kind.as_str(), it.sha256);
    Ok(())
}

pub fn ack(state: &State, ctx: &Ctx, id: &str) -> Result<()> {
    let it = state.ack(id)?;
    tracing::info!("acked {} {}", it.id, it.name);
    herdr::report_all_tokens(state, ctx);
    println!("{}", it.id);
    Ok(())
}

pub fn cat(state: &State, id: &str) -> Result<()> {
    let it = state.read_item(true, id)?;
    let p = state.outbox_payload(&it);
    let f = std::fs::File::open(&p).with_context(|| format!("payload missing for {id} (already acked?)"))?;
    let mut out = io::stdout().lock();
    io::copy(&mut io::BufReader::new(f), &mut out)?;
    Ok(())
}
