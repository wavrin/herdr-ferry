//! Server-side queue state: outbox, inbox, history. All under
//! `$HERDR_PLUGIN_STATE_DIR` when run by Herdr, else `~/.herdr-ferry/`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::{self, now_ms};

pub const CLAIM_TIMEOUT_MS: u64 = 10 * 60 * 1000;
pub const DEDUPE_WINDOW_MS: u64 = 2000;
pub const OUTBOX_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_ACKED_SIDECARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    File,
    Dir,
    ClipboardText,
    ClipboardImage,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Kind> {
        Ok(match s {
            "file" => Kind::File,
            "dir" => Kind::Dir,
            "clipboard-text" => Kind::ClipboardText,
            "clipboard-image" => Kind::ClipboardImage,
            _ => bail!("unknown kind {s}"),
        })
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::ClipboardText => "clipboard-text",
            Kind::ClipboardImage => "clipboard-image",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemState {
    Queued,
    Claimed,
    Acked,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub pane_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub mime: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub source: Source,
    pub created_unix_ms: u64,
    pub state: ItemState,
    #[serde(default)]
    pub claimed_unix_ms: Option<u64>,
    /// "outbox" or "inbox"; only present in history.jsonl
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

impl Item {
    pub fn new(kind: Kind, name: &str, size: u64, sha256: &str, mime: &str, note: Option<String>, source: Source) -> Item {
        Item {
            id: ulid::Ulid::new().to_string(),
            kind,
            name: name.to_string(),
            size,
            sha256: sha256.to_string(),
            mime: mime.to_string(),
            note,
            source,
            created_unix_ms: now_ms(),
            state: ItemState::Queued,
            claimed_unix_ms: None,
            direction: None,
        }
    }

    /// Filename of the payload inside the outbox item dir.
    pub fn outbox_payload_name(&self) -> String {
        match self.kind {
            Kind::Dir => format!("{}.tar.gz", self.name),
            _ => "payload".to_string(),
        }
    }
}

pub struct State {
    pub root: PathBuf,
}

impl State {
    pub fn open() -> Result<State> {
        let root = match std::env::var_os("HERDR_PLUGIN_STATE_DIR") {
            Some(d) if !d.is_empty() => PathBuf::from(d),
            _ => util::home_dir().join(".herdr-ferry"),
        };
        for sub in ["outbox", "inbox"] {
            fs::create_dir_all(root.join(sub)).with_context(|| format!("create {}", root.join(sub).display()))?;
        }
        Ok(State { root })
    }

    pub fn outbox_dir(&self) -> PathBuf {
        self.root.join("outbox")
    }
    pub fn inbox_dir(&self) -> PathBuf {
        self.root.join("inbox")
    }
    pub fn history_path(&self) -> PathBuf {
        self.root.join("history.jsonl")
    }
    pub fn item_dir(&self, outbox: bool, id: &str) -> PathBuf {
        if outbox { self.outbox_dir().join(id) } else { self.inbox_dir().join(id) }
    }

    pub fn write_item(&self, outbox: bool, item: &Item) -> Result<()> {
        let dir = self.item_dir(outbox, &item.id);
        fs::create_dir_all(&dir)?;
        let tmp = dir.join("item.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(item)?)?;
        fs::rename(&tmp, dir.join("item.json"))?;
        Ok(())
    }

    pub fn read_item(&self, outbox: bool, id: &str) -> Result<Item> {
        let p = self.item_dir(outbox, id).join("item.json");
        let bytes = fs::read(&p).with_context(|| format!("no such item {id}"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn list_dir(&self, outbox: bool) -> Result<Vec<Item>> {
        let dir = if outbox { self.outbox_dir() } else { self.inbox_dir() };
        let mut items = Vec::new();
        for e in fs::read_dir(&dir)? {
            let e = e?;
            if !e.file_type()?.is_dir() {
                continue;
            }
            let p = e.path().join("item.json");
            match fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Item>(&b).ok()) {
                Some(it) => items.push(it),
                None => tracing::debug!("skipping unreadable item {}", p.display()),
            }
        }
        items.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(items)
    }

    /// Outbox items, applying the claim-timeout recovery rule (claimed > 10 min → queued).
    pub fn list_outbox(&self) -> Result<Vec<Item>> {
        let mut items = self.list_dir(true)?;
        let now = now_ms();
        for it in items.iter_mut() {
            if it.state == ItemState::Claimed {
                let since = now.saturating_sub(it.claimed_unix_ms.unwrap_or(0));
                if since > CLAIM_TIMEOUT_MS {
                    tracing::info!("reverting stale claim on {} ({}s)", it.id, since / 1000);
                    it.state = ItemState::Queued;
                    it.claimed_unix_ms = None;
                    self.write_item(true, it)?;
                }
            }
        }
        Ok(items)
    }

    pub fn queued_outbox(&self) -> Result<Vec<Item>> {
        Ok(self.list_outbox()?.into_iter().filter(|i| i.state == ItemState::Queued).collect())
    }

    pub fn list_inbox(&self) -> Result<Vec<Item>> {
        self.list_dir(false)
    }

    pub fn outbox_payload(&self, item: &Item) -> PathBuf {
        self.item_dir(true, &item.id).join(item.outbox_payload_name())
    }
    pub fn inbox_payload(&self, item: &Item) -> PathBuf {
        self.item_dir(false, &item.id).join(&item.name)
    }

    /// Bytes currently held by un-acked outbox payloads.
    pub fn outbox_bytes(&self) -> Result<u64> {
        Ok(self.list_outbox()?.iter().filter(|i| i.state != ItemState::Acked).map(|i| i.size).sum())
    }

    pub fn find_dedupe(&self, sha256: &str) -> Result<Option<Item>> {
        let now = now_ms();
        Ok(self
            .list_outbox()?
            .into_iter()
            .find(|i| i.state == ItemState::Queued && i.sha256 == sha256 && now.saturating_sub(i.created_unix_ms) <= DEDUPE_WINDOW_MS))
    }

    pub fn claim(&self, id: &str) -> Result<Item> {
        let mut it = self.read_item(true, id)?;
        match it.state {
            ItemState::Acked => bail!("{id} already acked"),
            ItemState::Claimed if now_ms().saturating_sub(it.claimed_unix_ms.unwrap_or(0)) <= CLAIM_TIMEOUT_MS => {
                bail!("{id} already claimed")
            }
            _ => {}
        }
        it.state = ItemState::Claimed;
        it.claimed_unix_ms = Some(now_ms());
        self.write_item(true, &it)?;
        Ok(it)
    }

    pub fn ack(&self, id: &str) -> Result<Item> {
        let mut it = self.read_item(true, id)?;
        if it.state == ItemState::Acked {
            return Ok(it);
        }
        let payload = self.outbox_payload(&it);
        if payload.exists() {
            fs::remove_file(&payload)?;
        }
        it.state = ItemState::Acked;
        self.write_item(true, &it)?;
        self.append_history(&it, "outbox")?;
        self.prune_outbox()?;
        Ok(it)
    }

    pub fn append_history(&self, item: &Item, direction: &str) -> Result<()> {
        let mut it = item.clone();
        it.direction = Some(direction.to_string());
        let mut f = fs::OpenOptions::new().create(true).append(true).open(self.history_path())?;
        f.write_all(serde_json::to_string(&it)?.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(())
    }

    /// Keep at most MAX_ACKED_SIDECARS acked outbox sidecars.
    pub fn prune_outbox(&self) -> Result<()> {
        let mut acked: Vec<Item> = self.list_dir(true)?.into_iter().filter(|i| i.state == ItemState::Acked).collect();
        if acked.len() <= MAX_ACKED_SIDECARS {
            return Ok(());
        }
        acked.sort_by(|a, b| a.id.cmp(&b.id)); // ULID = chronological
        for it in acked.iter().take(acked.len() - MAX_ACKED_SIDECARS) {
            let _ = fs::remove_dir_all(self.item_dir(true, &it.id));
        }
        Ok(())
    }

    pub fn read_history(&self, n: usize) -> Result<Vec<Item>> {
        let p = self.history_path();
        if !p.exists() {
            return Ok(vec![]);
        }
        let text = fs::read_to_string(p)?;
        let mut all: Vec<Item> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let keep = all.len().saturating_sub(n);
        Ok(all.drain(keep..).collect())
    }

    pub fn print_history(&self, n: usize) -> Result<()> {
        let items = self.read_history(n)?;
        if items.is_empty() {
            println!("(no transfers yet)");
        }
        for it in items {
            println!(
                "{}  {:<6} {:<15} {:>8}  {}{}",
                it.id,
                it.direction.as_deref().unwrap_or("?"),
                it.kind.as_str(),
                util::human_size(it.size),
                it.name,
                it.note.as_ref().map(|n| format!("  — {n}")).unwrap_or_default()
            );
        }
        Ok(())
    }
}

pub fn print_items(items: &[Item], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(items)?);
        return Ok(());
    }
    if items.is_empty() {
        println!("(empty)");
    }
    for it in items {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            it.id,
            it.name,
            it.size,
            it.kind.as_str(),
            serde_json::to_value(it.state)?.as_str().unwrap_or("?"),
            util::human_age(it.created_unix_ms)
        );
    }
    Ok(())
}

