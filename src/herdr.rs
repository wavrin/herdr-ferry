//! Talking to Herdr: env vars, invocation context, and CLI wrappers over HERDR_BIN_PATH.
//! Every function degrades gracefully when Herdr isn't present (plain shell / over SSH).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::state::{ItemState, Source, State};
use crate::util::run;

/// Shape of HERDR_PLUGIN_CONTEXT_JSON (PluginInvocationContext in the API schema, verified 0.8.2).
#[derive(Debug, Default, Clone, Deserialize)]
#[allow(dead_code)]
pub struct InvocationContext {
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_cwd: Option<String>,
    #[serde(default)]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub focused_pane_cwd: Option<String>,
    #[serde(default)]
    pub focused_pane_agent: Option<String>,
    #[serde(default)]
    pub selected_text: Option<String>,
    #[serde(default)]
    pub clicked_url: Option<String>,
    #[serde(default)]
    pub invocation_source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Ctx {
    pub bin: PathBuf,
    pub under_herdr: bool,
    pub plugin_id: String,
    pub context: InvocationContext,
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
}

impl Ctx {
    pub fn from_env() -> Ctx {
        let env = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        let bin = env("HERDR_BIN_PATH").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("herdr"));
        let under_herdr = env("HERDR_ENV").is_some() || env("HERDR_BIN_PATH").is_some();
        let context: InvocationContext = env("HERDR_PLUGIN_CONTEXT_JSON")
            .and_then(|j| serde_json::from_str(&j).map_err(|e| tracing::warn!("bad HERDR_PLUGIN_CONTEXT_JSON: {e}")).ok())
            .unwrap_or_default();
        // The picker pane gets its context via --env from send-from-context.
        let pane_id = context.focused_pane_id.clone().or_else(|| env("FERRY_PANE_ID")).or_else(|| env("HERDR_PANE_ID"));
        let workspace_id = context.workspace_id.clone().or_else(|| env("FERRY_WORKSPACE_ID")).or_else(|| env("HERDR_WORKSPACE_ID"));
        Ctx {
            bin,
            under_herdr,
            plugin_id: env("HERDR_PLUGIN_ID").unwrap_or_else(|| "herdr-ferry".into()),
            context,
            pane_id,
            workspace_id,
        }
    }

    pub fn herdr(&self, args: &[&str]) -> Result<String> {
        tracing::debug!("herdr {}", args.join(" "));
        run(&self.bin, args)
    }

    pub fn herdr_json(&self, args: &[&str]) -> Result<Value> {
        let out = self.herdr(args)?;
        let v: Value = serde_json::from_str(&out).with_context(|| format!("herdr {} did not return JSON", args.join(" ")))?;
        Ok(v.get("result").cloned().unwrap_or(v))
    }

    /// Is a Herdr server reachable at all?
    pub fn server_alive(&self) -> bool {
        self.herdr(&["workspace", "list"]).is_ok()
    }

    pub fn selected_text(&self) -> Option<String> {
        self.context.selected_text.clone().or_else(|| std::env::var("FERRY_SELECTED").ok()).filter(|s| !s.trim().is_empty())
    }

    /// Focused pane id: context → env → `pane list` focused entry.
    pub fn resolve_pane(&self) -> Result<String> {
        if let Some(p) = &self.pane_id {
            return Ok(p.clone());
        }
        let v = self.herdr_json(&["pane", "list"])?;
        let panes = v.get("panes").and_then(|p| p.as_array()).cloned().unwrap_or_default();
        let focused = panes.iter().find(|p| p.get("focused").and_then(|f| f.as_bool()).unwrap_or(false));
        match focused.and_then(|p| p.get("pane_id")).and_then(|s| s.as_str()) {
            Some(id) => Ok(id.to_string()),
            None => bail!("no focused pane (is Herdr attached?)"),
        }
    }

    pub fn pane_info(&self, pane: &str) -> Result<Value> {
        let v = self.herdr_json(&["pane", "get", pane])?;
        Ok(v.get("pane").cloned().unwrap_or(v))
    }

    /// cwd for resolving relative paths: context.focused_pane_cwd → FERRY_CWD → pane get → process cwd.
    pub fn pane_cwd(&self) -> PathBuf {
        if let Some(c) = &self.context.focused_pane_cwd {
            return PathBuf::from(c);
        }
        if let Ok(c) = std::env::var("FERRY_CWD") {
            if !c.is_empty() {
                return PathBuf::from(c);
            }
        }
        if self.under_herdr {
            if let Some(p) = &self.pane_id {
                if let Ok(info) = self.pane_info(p) {
                    for k in ["foreground_cwd", "cwd"] {
                        if let Some(c) = info.get(k).and_then(|c| c.as_str()) {
                            return PathBuf::from(c);
                        }
                    }
                }
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    pub fn source(&self) -> Source {
        let mut s = Source {
            pane_id: self.pane_id.clone(),
            workspace_id: self.workspace_id.clone(),
            agent: self.context.focused_pane_agent.clone().or_else(|| std::env::var("FERRY_AGENT").ok()),
            cwd: Some(self.pane_cwd().display().to_string()),
        };
        if s.workspace_id.is_none() {
            if let Some(p) = &s.pane_id {
                s.workspace_id = p.split(':').next().map(|w| w.to_string());
            }
        }
        s
    }

    pub fn pane_read(&self, pane: &str, lines: u32) -> Result<String> {
        self.herdr(&["pane", "read", pane, "--source", "recent-unwrapped", "--lines", &lines.to_string()])
    }

    /// pane.send_text — inserts text, never presses Enter.
    pub fn send_text(&self, pane: &str, text: &str) -> Result<()> {
        self.herdr(&["pane", "send-text", pane, text])?;
        Ok(())
    }

    pub fn workspace_ids(&self) -> Result<Vec<String>> {
        let v = self.herdr_json(&["workspace", "list"])?;
        Ok(v.get("workspaces")
            .and_then(|w| w.as_array())
            .map(|ws| ws.iter().filter_map(|w| w.get("workspace_id").and_then(|s| s.as_str()).map(String::from)).collect())
            .unwrap_or_default())
    }

    pub fn report_token(&self, workspace: &str, count: usize) -> Result<()> {
        let mut args = vec!["workspace", "report-metadata", workspace, "--source", "ferry"];
        let val = count.to_string();
        let tok = format!("ferry={val}");
        if count == 0 {
            args.extend(["--clear-token", "ferry"]);
        } else {
            args.extend(["--token", &tok]);
        }
        self.herdr(&args)?;
        Ok(())
    }

    /// Open the picker popup, passing our context along as env so it works without
    /// relying on Herdr forwarding HERDR_PLUGIN_CONTEXT_JSON to pane commands.
    pub fn open_picker(&self) -> Result<()> {
        let mut args: Vec<String> = vec!["plugin".into(), "pane".into(), "open".into(), "--plugin".into(), self.plugin_id.clone(), "--entrypoint".into(), "picker".into(), "--focus".into()];
        if let Some(ws) = &self.workspace_id {
            args.extend(["--workspace".into(), ws.clone()]);
        }
        // popup/overlay panes always attach to the active pane; --target-pane is rejected for them
        if let Some(p) = &self.pane_id {
            args.extend(["--env".into(), format!("FERRY_PANE_ID={p}")]);
        }
        if let Some(ws) = &self.workspace_id {
            args.extend(["--env".into(), format!("FERRY_WORKSPACE_ID={ws}")]);
        }
        let cwd = self.pane_cwd();
        args.extend(["--env".into(), format!("FERRY_CWD={}", cwd.display())]);
        if let Some(a) = &self.context.focused_pane_agent {
            args.extend(["--env".into(), format!("FERRY_AGENT={a}")]);
        }
        if let Some(sel) = self.selected_text() {
            args.extend(["--env".into(), format!("FERRY_SELECTED={}", sel.lines().next().unwrap_or("").trim())]);
        }
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        self.herdr(&a)?;
        Ok(())
    }
}

/// Recompute `$ferry` for every workspace. Never fails: no Herdr → no-op.
pub fn report_all_tokens(state: &State, ctx: &Ctx) {
    let items = match state.list_outbox() {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("status-token: {e}");
            return;
        }
    };
    let workspaces = match ctx.workspace_ids() {
        Ok(w) => w,
        Err(e) => {
            tracing::debug!("status-token: no Herdr server ({e:#})");
            return;
        }
    };
    for ws in workspaces {
        let n = items
            .iter()
            .filter(|i| i.state != ItemState::Acked)
            .filter(|i| i.source.workspace_id.as_deref() == Some(ws.as_str()))
            .count();
        if let Err(e) = ctx.report_token(&ws, n) {
            tracing::warn!("report token for {ws}: {e:#}");
        }
    }
}

pub fn path_exists_rel(base: &Path, s: &str) -> Option<PathBuf> {
    let p = crate::util::expand_tilde(Path::new(s.trim()));
    let full = if p.is_absolute() { p } else { base.join(p) };
    if full.exists() { Some(full) } else { None }
}

#[allow(dead_code)]
pub fn herdr_bin_exists(ctx: &Ctx) -> bool {
    Command::new(&ctx.bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}
