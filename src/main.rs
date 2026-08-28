mod client;
mod doctor;
mod herdr;
mod inbox;
mod logging;
mod outbox;
mod picker;
mod send;
mod ssh;
mod state;
mod util;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "herdr-ferry", version, about = "Move files and clipboard between the Herdr box and your laptop over SSH")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// [server] Stage file(s)/dir(s) into the outbox for the laptop to pull
    Send {
        paths: Vec<PathBuf>,
        /// Human note shown in the laptop notification
        #[arg(long)]
        note: Option<String>,
        /// Send the server clipboard (text or image) instead of a path
        #[arg(long)]
        clip: bool,
        /// Send even if the name matches the secrets deny-list
        #[arg(long)]
        force: bool,
        /// Allow paths outside $HOME
        #[arg(long)]
        allow_outside_home: bool,
    },
    /// [server, plugin action] Send the selected path, or open the picker popup
    SendFromContext,
    /// [server] Block until an unclaimed outbox item exists (exit 0) or timeout (exit 2)
    OutboxWait {
        #[arg(long, default_value_t = 55)]
        timeout: u64,
    },
    /// [server] List outbox items
    OutboxList {
        #[arg(long)]
        json: bool,
    },
    /// [server] Mark an item in-flight
    OutboxClaim { id: String },
    /// [server] Mark an item delivered; deletes the payload, keeps the sidecar
    OutboxAck { id: String },
    /// [server] Stream an item's payload to stdout
    #[command(hide = true)]
    OutboxCat { id: String },
    /// [server] List inbox items
    InboxList {
        #[arg(long)]
        json: bool,
    },
    /// [server] Paste an inbox item's path into the focused pane (default: latest)
    InboxPaste { id: Option<String> },
    /// [server] Receive a pushed payload on stdin into the inbox
    #[command(hide = true)]
    InboxReceive {
        #[arg(long)]
        name: String,
        #[arg(long)]
        sha256: String,
        #[arg(long)]
        size: u64,
        #[arg(long, default_value = "file")]
        kind: String,
        #[arg(long)]
        mime: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Show the last N transfers
    History {
        #[arg(short, default_value_t = 20)]
        n: usize,
    },
    /// [client] Long-poll the server and deliver items as they appear
    Watch {
        #[arg(long)]
        alias: Option<String>,
        #[arg(long)]
        dest: Option<PathBuf>,
        /// Open each received file with open/xdg-open
        #[arg(long)]
        open: bool,
    },
    /// [client] Drain the outbox once
    Pull {
        #[arg(long)]
        alias: Option<String>,
        #[arg(long)]
        dest: Option<PathBuf>,
        #[arg(long)]
        open: bool,
    },
    /// [client] Push file(s) to the server inbox
    Push {
        paths: Vec<PathBuf>,
        #[arg(long)]
        alias: Option<String>,
        /// Also paste the inbox path into the focused agent on the server
        #[arg(long)]
        paste: bool,
        #[arg(long)]
        note: Option<String>,
    },
    /// [server, plugin pane] Interactive file picker
    Picker {
        /// Print candidates and exit (no TUI)
        #[arg(long, hide = true)]
        list: bool,
    },
    /// [server] Report the $ferry sidebar token for every workspace
    StatusToken,
    /// Check prerequisites on this machine
    Doctor {
        #[arg(long)]
        alias: Option<String>,
    },
}

fn main() -> ExitCode {
    logging::init();
    let cli = Cli::parse();
    match run(cli.cmd) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            tracing::info!("failed: {e:#}");
            eprintln!("herdr-ferry: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cmd: Cmd) -> anyhow::Result<u8> {
    let state = state::State::open()?;
    let ctx = herdr::Ctx::from_env();
    match cmd {
        Cmd::Send { paths, note, clip, force, allow_outside_home } => {
            let opts = send::SendOpts { note, clip, force, allow_outside_home };
            let ids = send::send(&state, &ctx, &paths, &opts)?;
            for id in ids {
                println!("{id}");
            }
            Ok(0)
        }
        Cmd::SendFromContext => send::send_from_context(&state, &ctx),
        Cmd::OutboxWait { timeout } => outbox::wait(&state, timeout),
        Cmd::OutboxList { json } => {
            outbox::list(&state, json)?;
            Ok(0)
        }
        Cmd::OutboxClaim { id } => {
            outbox::claim(&state, &id)?;
            Ok(0)
        }
        Cmd::OutboxAck { id } => {
            outbox::ack(&state, &ctx, &id)?;
            Ok(0)
        }
        Cmd::OutboxCat { id } => {
            outbox::cat(&state, &id)?;
            Ok(0)
        }
        Cmd::InboxList { json } => {
            inbox::list(&state, json)?;
            Ok(0)
        }
        Cmd::InboxPaste { id } => {
            inbox::paste(&state, &ctx, id.as_deref())?;
            Ok(0)
        }
        Cmd::InboxReceive { name, sha256, size, kind, mime, note } => {
            let id = inbox::receive(&state, &name, &sha256, size, &kind, mime, note)?;
            println!("{id}");
            Ok(0)
        }
        Cmd::History { n } => {
            state.print_history(n)?;
            Ok(0)
        }
        Cmd::Watch { alias, dest, open } => client::watch(alias, dest, open),
        Cmd::Pull { alias, dest, open } => client::pull(alias, dest, open),
        Cmd::Push { paths, alias, paste, note } => client::push(alias, &paths, paste, note),
        Cmd::Picker { list } => picker::run(&state, &ctx, list),
        Cmd::StatusToken => {
            herdr::report_all_tokens(&state, &ctx);
            Ok(0)
        }
        Cmd::Doctor { alias } => doctor::run(&state, &ctx, alias),
    }
}
