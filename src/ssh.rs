//! Client-side transport: everything goes through `ssh <alias>` and the user's own config.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::util::{self, sh_quote};

#[derive(Debug, Default, Deserialize)]
pub struct ClientConfig {
    pub alias: Option<String>,
    pub remote_bin: Option<String>,
    pub dest: Option<PathBuf>,
}

pub fn config_path() -> PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.config_dir().join("herdr-ferry").join("client.toml"))
        .unwrap_or_else(|| util::home_dir().join(".config/herdr-ferry/client.toml"))
}

pub fn load_config() -> ClientConfig {
    let p = config_path();
    match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s).map_err(|e| tracing::warn!("{}: {e}", p.display())).unwrap_or_default(),
        Err(_) => ClientConfig::default(),
    }
}

pub struct Remote {
    pub alias: String,
    pub remote_bin: String,
}

/// Runs in the remote login shell (often a minimal PATH), so look in the usual places and
/// finally ask Herdr itself where the plugin lives (covers both `plugin install` and `plugin link`).
const REMOTE_BIN_CANDIDATES: &str = r#"command -v herdr-ferry 2>/dev/null && exit 0
for p in "$HOME/.local/bin/herdr-ferry" "$HOME/.cargo/bin/herdr-ferry" "$HOME/.herdr-ferry/bin/herdr-ferry" "$HOME"/.config/herdr/plugins/github/*herdr-ferry*/target/release/herdr-ferry; do
  [ -x "$p" ] && { echo "$p"; exit 0; }
done
H=$(command -v herdr || ls "$HOME/.local/bin/herdr" "$HOME/.cargo/bin/herdr" /opt/homebrew/bin/herdr /usr/local/bin/herdr 2>/dev/null | head -1)
[ -n "$H" ] && root=$("$H" plugin list --json 2>/dev/null | tr ',' '\n' | grep -A1 '"plugin_id":"herdr-ferry"' | grep -o '"plugin_root":"[^"]*"' | head -1 | cut -d'"' -f4)
[ -n "$root" ] && [ -x "$root/target/release/herdr-ferry" ] && { echo "$root/target/release/herdr-ferry"; exit 0; }
exit 3"#;

impl Remote {
    pub fn resolve(alias: Option<String>) -> Result<Remote> {
        let cfg = load_config();
        let alias = alias
            .or_else(|| std::env::var("HERDR_FERRY_ALIAS").ok().filter(|s| !s.is_empty()))
            .or(cfg.alias)
            .with_context(|| format!("no ssh alias: pass --alias, set HERDR_FERRY_ALIAS, or write `alias = \"...\"` to {}", config_path().display()))?;
        let remote_bin = match cfg.remote_bin {
            Some(b) => b,
            None => discover_remote_bin(&alias)?,
        };
        Ok(Remote { alias, remote_bin })
    }

    fn ssh_cmd(&self, args: &[&str]) -> Command {
        let mut remote = sh_quote(&self.remote_bin);
        for a in args {
            remote.push(' ');
            remote.push_str(&sh_quote(a));
        }
        let mut c = Command::new("ssh");
        c.arg("-o").arg("BatchMode=yes").arg(&self.alias).arg(remote);
        c
    }

    /// Run a remote ferry subcommand; returns (exit code, stdout).
    pub fn run(&self, args: &[&str]) -> Result<(i32, String)> {
        let out = self.ssh_cmd(args).stdin(Stdio::null()).stderr(Stdio::inherit()).output().context("spawn ssh")?;
        Ok((out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).to_string()))
    }

    pub fn run_ok(&self, args: &[&str]) -> Result<String> {
        let (code, out) = self.run(args)?;
        if code != 0 {
            bail!("remote `herdr-ferry {}` exited {code}", args.join(" "));
        }
        Ok(out)
    }

    /// Stream remote stdout into `w`.
    pub fn run_to_writer(&self, args: &[&str], w: &mut impl Write) -> Result<u64> {
        let mut child: Child = self.ssh_cmd(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().context("spawn ssh")?;
        let mut stdout = child.stdout.take().unwrap();
        let n = io::copy(&mut stdout, w)?;
        let st = child.wait()?;
        if !st.success() {
            bail!("remote `herdr-ferry {}` exited {}", args.join(" "), st);
        }
        Ok(n)
    }

    /// Stream a local file into the remote command's stdin; returns remote stdout.
    pub fn run_from_file(&self, args: &[&str], file: &Path) -> Result<String> {
        let mut child = self.ssh_cmd(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().context("spawn ssh")?;
        {
            let mut stdin = child.stdin.take().unwrap();
            let mut f = File::open(file)?;
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                stdin.write_all(&buf[..n])?;
            }
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("remote `herdr-ferry {}` exited {}", args.join(" "), out.status);
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn control_master_active(&self) -> bool {
        Command::new("ssh").args(["-O", "check", &self.alias]).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
    }
}

pub fn discover_remote_bin(alias: &str) -> Result<String> {
    let out = Command::new("ssh").args(["-o", "BatchMode=yes", alias, REMOTE_BIN_CANDIDATES]).stdin(Stdio::null()).output().context("spawn ssh")?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match out.status.code() {
        Some(0) if !path.is_empty() => Ok(path),
        Some(3) => bail!("herdr-ferry not found on `{alias}`: install/link the plugin there, or symlink target/release/herdr-ferry into ~/.local/bin, or set remote_bin in client.toml"),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let hint = if err.contains("Host key verification failed") {
                " — run `ssh {alias}` once interactively to accept the host key".replace("{alias}", alias)
            } else if err.contains("Permission denied") {
                " — key auth is required (BatchMode); add your key to the server's authorized_keys".into()
            } else {
                String::new()
            };
            bail!("ssh to `{alias}` failed: {err}{hint}")
        }
    }
}

/// `ssh -G alias` resolves the hostname; lets doctor tell alias-typos from network failures.
pub fn alias_hostname(alias: &str) -> Option<String> {
    let out = util::run("ssh", &["-G", alias]).ok()?;
    out.lines().find_map(|l| l.strip_prefix("hostname ").map(|h| h.trim().to_string()))
}
