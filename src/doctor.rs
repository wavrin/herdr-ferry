use anyhow::Result;

use crate::herdr::Ctx;
use crate::ssh::{self, load_config, Remote};
use crate::state::State;
use crate::util;

fn line(ok: bool, what: &str, detail: &str) {
    println!("{} {:<28} {}", if ok { "✓" } else { "✗" }, what, detail);
}

pub fn run(state: &State, ctx: &Ctx, alias: Option<String>) -> Result<u8> {
    let mut bad = 0;
    let mut chk = |ok: bool, what: &str, detail: &str| {
        line(ok, what, detail);
        if !ok {
            bad += 1;
        }
    };
    println!("herdr-ferry {} on {}", crate::VERSION, std::env::consts::OS);
    println!();

    println!("server side");
    chk(state.root.exists(), "state dir", &state.root.display().to_string());
    let bin_ok = util::run(&ctx.bin, &["--version"]).ok();
    chk(bin_ok.is_some(), "herdr binary", &bin_ok.clone().unwrap_or_else(|| format!("{} not runnable — fine on a client-only laptop", ctx.bin.display())));
    if bin_ok.is_some() {
        chk(ctx.server_alive(), "herdr server", if ctx.server_alive() { "responding" } else { "not running (start herdr)" });
    }
    let this = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let on_path = util::which("herdr-ferry");
    let path_hint = if on_path { "ok (clients can find it over ssh)".to_string() } else { format!("add {} to PATH or symlink it", this.map(|p| p.display().to_string()).unwrap_or_default()) };
    chk(on_path, "herdr-ferry on PATH", &path_hint);
    chk(util::which("pbpaste") || util::which("wl-paste") || util::which("xclip"), "clipboard read", "pbpaste / wl-paste / xclip (for send --clip)");

    println!();
    println!("client side");
    chk(util::which("ssh"), "ssh", "");
    let cfg = load_config();
    let alias = alias.or_else(|| std::env::var("HERDR_FERRY_ALIAS").ok()).or(cfg.alias.clone());
    match alias {
        None => chk(false, "ssh alias", &format!("none: pass --alias, set HERDR_FERRY_ALIAS, or write alias = \"...\" to {}", ssh::config_path().display())),
        Some(a) => {
            let host = ssh::alias_hostname(&a);
            let detail = match &host {
                Some(h) if h == &a => format!("{a} (plain hostname, not a ~/.ssh/config alias — fine)"),
                Some(h) => format!("{a} → {h}"),
                None => format!("{a}: ssh cannot resolve it (typo, or missing from ~/.ssh/config)"),
            };
            chk(host.is_some(), "ssh alias", &detail);
            match Remote::resolve(Some(a.clone())) {
                Ok(r) => {
                    chk(true, "remote herdr-ferry", &r.remote_bin);
                    match r.run(&["--version"]) {
                        Ok((0, v)) => {
                            let v = v.trim().trim_start_matches("herdr-ferry ").to_string();
                            chk(v == crate::VERSION, "remote version", &format!("{v} (local {})", crate::VERSION));
                        }
                        _ => chk(false, "remote version", "could not run remote binary"),
                    }
                    chk(r.control_master_active(), "ssh ControlMaster", if r.control_master_active() { "active" } else { "not active — add `ControlMaster auto` + `ControlPersist 10m` to the alias for ~0ms polls" });
                }
                Err(e) => chk(false, "remote herdr-ferry", &format!("{e:#}")),
            }
        }
    }
    let clip_w = if util::os_is_macos() { util::which("pbcopy") && util::which("osascript") } else { util::which("wl-copy") || util::which("xclip") };
    chk(clip_w, "clipboard write", if util::os_is_macos() { "pbcopy + osascript" } else { "wl-copy / xclip" });
    let notif = if util::os_is_macos() { util::which("osascript") } else { util::which("notify-send") };
    chk(notif, "notifications", if util::os_is_macos() { if util::which("terminal-notifier") { "terminal-notifier (clickable)" } else { "osascript (install terminal-notifier for clickable toasts)" } } else { "notify-send" });

    println!();
    if bad == 0 {
        println!("all good");
        Ok(0)
    } else {
        println!("{bad} problem(s)");
        Ok(1)
    }
}
