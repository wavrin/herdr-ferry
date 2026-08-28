use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn sha256_bytes(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else {
        format!("{v:.1}{}", U[i])
    }
}

pub fn human_age(unix_ms: u64) -> String {
    let now = now_ms();
    let s = now.saturating_sub(unix_ms) / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

pub fn home_dir() -> PathBuf {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~") {
        home_dir().join(rest)
    } else {
        p.to_path_buf()
    }
}

/// Secrets deny-list from spec §9. Returns the matching rule.
pub fn denied_reason(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with(".env") {
        return Some(".env*");
    }
    if lower.ends_with(".pem") {
        return Some("*.pem");
    }
    if lower.starts_with("id_") {
        return Some("id_* (ssh key)");
    }
    if lower.ends_with(".key") {
        return Some("*.key");
    }
    if lower == ".netrc" {
        return Some(".netrc");
    }
    if lower.contains("credentials") {
        return Some("*credentials*");
    }
    None
}

pub fn which(bin: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {} >/dev/null 2>&1", sh_quote(bin))])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Run a command, return trimmed stdout; error carries stderr.
pub fn run(bin: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Result<String> {
    let bin = bin.as_ref();
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("spawn {}", bin.to_string_lossy()))?;
    if !out.status.success() {
        bail!(
            "{} {} failed ({}): {}",
            bin.to_string_lossy(),
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

pub fn run_ok(bin: &str, args: &[&str]) -> bool {
    Command::new(bin).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

pub fn file_size(p: &Path) -> Result<u64> {
    Ok(std::fs::metadata(p)?.len())
}

pub fn mime_for(name: &str) -> String {
    mime_guess::from_path(name).first_or_octet_stream().essence_str().to_string()
}

pub fn is_image_mime(m: &str) -> bool {
    m.starts_with("image/")
}

/// Pick `dir/name`, or `dir/stem-2.ext`, `-3`... if taken.
pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.to_string());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    // "foo.tar.gz" → keep both extensions together
    let (stem, ext) = if stem.ends_with(".tar") && ext == ".gz" {
        (stem.trim_end_matches(".tar").to_string(), ".tar.gz".to_string())
    } else {
        (stem, ext)
    };
    for i in 2.. {
        let cand = dir.join(format!("{stem}-{i}{ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    unreachable!()
}


pub fn os_is_macos() -> bool {
    cfg!(target_os = "macos")
}

