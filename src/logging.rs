use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::Mutex;

use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

/// stderr gets warnings (or RUST_LOG); under Herdr everything at info+ also goes to
/// `$HERDR_PLUGIN_STATE_DIR/ferry.log` so `herdr plugin log list` and the file agree.
pub fn init() {
    let explicit = std::env::var_os("RUST_LOG").is_some();
    let global = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr_level = if explicit { LevelFilter::TRACE } else { LevelFilter::WARN };
    let stderr = fmt::layer()
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .with_filter(stderr_level);

    let file = std::env::var_os("HERDR_PLUGIN_STATE_DIR").and_then(|d| {
        fs::create_dir_all(&d).ok()?;
        OpenOptions::new().create(true).append(true).open(Path::new(&d).join("ferry.log")).ok()
    });

    let reg = tracing_subscriber::registry().with(global).with(stderr);
    match file {
        Some(f) => reg.with(fmt::layer().with_writer(Mutex::new(f)).with_ansi(false).with_target(false)).init(),
        None => reg.init(),
    }
}
