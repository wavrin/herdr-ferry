herdr-ferry — PRD / Technical Spec

Working name: herdr-ferry (plugin id <github-handle>.ferry)
One-liner: Move files and clipboard content between the machine Herdr runs on and the machine you're attached from — over the SSH you already have, no cloud bucket.
Language: Rust, single crate, one binary with subcommands (runs on both sides).
Status: v0.1 spec, 2026-08-28. Items marked ⚠ VERIFY are assumptions about Herdr that must be checked against the installed binary before building on them.



1. Problem

Herdr's whole premise is that agents run on a box you're not sitting at. The moment an agent on that box produces something you need on the laptop — a screenshot, a generated PDF, a .xlsx, a log — or you need to hand the agent something from the laptop — a screenshot of a bug, a CSV, a design mockup — you drop out to scp, remember the path, and lose the flow.

Text is mostly solved (terminal copy, OSC 52). Files are not. The marketplace has one S3-based clipboard plugin (herdr-s3-clipboard) and nothing that uses the direct link you already have.

2. Goals





Send: from inside Herdr on the remote, pick a file the agent just produced and have it appear on the laptop within ~1s, with a notification and (for images) already on the laptop clipboard.



Fetch: from the laptop, push a file to the remote and have its path dropped into the focused agent's input, ready to reference.



Zero new infrastructure: only SSH (existing ~/.ssh/config alias) — Tailscale-friendly but not required.



Agent-usable: an agent inside a Herdr pane can call herdr-ferry send ./report.pdf and the user gets it. Same binary, same command.



Ship small: v0.1 in a weekend; Rust with a minimal dependency set; cargo build --release as the only build step.

3. Non-goals (v0.1)





Bidirectional continuous sync (this is not Syncthing / Mutagen).



Public URLs / sharing with third parties (herdr-tunnel and herdr-s3-clipboard cover that).



Windows as the remote side. Windows laptop as client is a stretch goal (see §11).



A GUI. Everything is TUI popups inside Herdr plus native OS notifications.



Replacing OSC 52 for plain text copy.

4. Vocabulary







Term



Meaning





Server



The machine Herdr's server runs on and the plugin is installed on (e.g. a Mac mini or Linux box on your LAN/tailnet).





Client



The machine you attach from (the laptop). Runs the same binary in client mode; no Herdr plugin install needed there.





Outbox



Server-side directory of items waiting to go to the client.





Inbox



Server-side directory of items the client has pushed to the server.





Item



One transfer: a payload (file, directory tarball, or clipboard blob) plus a JSON sidecar.

5. Users & stories

U1 — The user at the laptop, agent on the server.





"The agent said it saved out/dashboard.png. I want to see it." → prefix+f → picker shows recently mentioned/modified files → Enter → image is on my laptop clipboard and in ~/Downloads/herdr-ferry/, macOS toast says so.



"I screenshotted a bug. I want the agent to look at it." → on laptop: herdr-ferry push ~/Desktop/bug.png → on the server, the focused agent's input now contains ~/.herdr-ferry/inbox/bug.png (pasted, not submitted).



"Just give me everything the agent wrote in dist/." → picker, select the directory → arrives as dist.tar.gz, auto-extracted.

U2 — An agent inside a pane.





Agent finishes a report and runs herdr-ferry send report.pdf --note "Q3 summary". The user gets a notification on the laptop with the note.

U3 — The user on the phone (out of scope for transport, in scope for awareness).





Outbox count shows in the Herdr sidebar as $ferry: 2 so remote-monitoring plugins can surface it.

6. Architecture

  CLIENT (laptop)                                SERVER (runs Herdr + plugin)  
  ┌────────────────────────┐                     ┌────────────────────────────────────┐
  │ herdr-ferry watch      │  ssh alias (existing)│ Herdr server                       │
  │  ├─ long-poll outbox ──┼─────────────────────┼─▶ herdr-ferry outbox-wait          │
  │  ├─ scp/tar pull ──────┼─────────────────────┼─▶ ~/.herdr-ferry/outbox/<id>/      │
  │  ├─ write ~/Downloads  │                     │                                    │
  │  ├─ set clipboard      │                     │ Plugin (manifest actions/panes)    │
  │  └─ OS notification    │                     │  ├─ action: send   (picker popup)  │
  │                        │                     │  ├─ action: send-selection         │
  │ herdr-ferry push FILE ─┼─────────────────────┼─▶ ~/.herdr-ferry/inbox/<id>/       │
  │                        │                     │  ├─ action: paste-inbox            │
  └────────────────────────┘                     │  ├─ event: pane.exited → cleanup   │
                                                 │  └─ startup: reapply $ferry token  │
                                                 └────────────────────────────────────┘

Key decision: the client pulls; the server never needs to reach the client. This works over plain SSH to a NAT'd box, over Tailscale, or over a jump host, because the only connection is the one the user already has. The server side is therefore just a well-behaved queue plus Herdr UI. A "server pushes over tailnet" mode is a v0.2 optimization (§11).

Long-poll instead of polling: the client runs ssh <alias> herdr-ferry outbox-wait --timeout 55 in a loop. The server-side command blocks (fs watch on the outbox) until an item appears or the timeout elapses, then prints the item ids and exits. With SSH ControlMaster this costs nothing; without it, one SSH handshake per minute. Latency from "send" to "on laptop" is bounded by one round trip.

7. Components

7.1 One binary, two roles

herdr-ferry is a single Rust binary. Role is inferred from the subcommand; there is no daemon on the server side.







Subcommand



Runs on



Purpose





send <PATH>... [--note] [--clip]



server



Stage file(s)/dir(s) into outbox. Directories are tarred. --clip reads the server clipboard (pbpaste / wl-paste / xclip) instead of a path. Prints item id.





outbox-wait --timeout SECS



server



Block until ≥1 unclaimed item exists or timeout; print id\tname\tsize\tkind lines; exit 0 (items) / 2 (timeout).





outbox-list / outbox-claim <id> / outbox-ack <id>



server



Queue management. Claim = mark in-flight; ack = delete payload, keep sidecar for history.





inbox-list / inbox-paste [<id>]



server



List inbox; paste the item's path into the focused pane via HERDR_BIN_PATH pane send-text. Default: latest.





history [-n]



both



Last N transfers from sidecars.





watch [--alias A] [--dest D] [--open]



client



Long-poll loop; on items: claim → scp/tar pull → verify sha256 → ack → clipboard/notify/open.





pull [--alias A]



client



One-shot version of watch (drain outbox once).





push <PATH>... [--alias A] [--paste]



client



scp to server inbox, write sidecar, optionally invoke herdr-ferry inbox-paste on the server so the path lands in the agent input immediately.





picker



server (plugin pane)



TUI file picker (see 7.3). Wraps send.





status-token



server (plugin startup/event)



Report $ferry workspace token with outbox count.





doctor



both



Check: ssh alias resolves, remote binary present and same version, ControlMaster active, clipboard tool available, notification tool available.

7.2 Server-side state (all under HERDR_PLUGIN_STATE_DIR when invoked by Herdr, else ~/.herdr-ferry/)

outbox/<ulid>/payload         # the file, or <name>.tar.gz for directories
outbox/<ulid>/item.json
inbox/<ulid>/<original-name>
inbox/<ulid>/item.json
history.jsonl                 # appended on ack (outbox) or push (inbox)

item.json:

{
  "id": "01J6...ULID",
  "kind": "file" | "dir" | "clipboard-text" | "clipboard-image",
  "name": "dashboard.png",
  "size": 48213,
  "sha256": "…",
  "mime": "image/png",
  "note": "optional human note",
  "source": { "pane_id": "w1:p3", "workspace_id": "w1", "agent": "claude", "cwd": "/home/user/proj" },
  "created_unix_ms": 1724800000000,
  "state": "queued" | "claimed" | "acked",
  "claimed_unix_ms": null
}

Rules: ULID ids (sortable, no coordination). Claimed items older than 10 minutes revert to queued (client died mid-pull). send of the same path within 2s is deduped by sha256. Outbox hard cap 2 GiB total, oldest acked sidecars pruned past 500 entries.

Path resolution for send: relative paths resolve against foreground_cwd of the source pane when invoked as a plugin action (from HERDR_PLUGIN_CONTEXT_JSON), else against the process cwd. ⚠ VERIFY that context JSON includes a cwd for the focused pane; if not, call HERDR_BIN_PATH pane get <HERDR_PANE_ID> --json and read foreground_cwd / cwd.

7.3 Picker (plugin pane, placement = "popup", width = "70%", height = 18)

A minimal TUI (ratatui or plain crossterm) that lists candidates, fuzzy-filters as you type, multi-selects with Tab, sends on Enter, cancels on Esc.

Candidate sources, in order, deduped:





Selected text in the pane, if it looks like a path (from context JSON selected_text).



Paths mentioned in recent pane output — HERDR_BIN_PATH pane read <pane> --source recent-unwrapped --lines 300, regex for path-like tokens that exist on disk relative to pane cwd.



Recently modified files under pane cwd (mtime within 30 min, depth ≤ 4, respecting .gitignore via the ignore crate, excluding node_modules, target, .git).



A free-text path entry if nothing matches.

Show for each: relative path, size, age, and a glyph for image/pdf/archive/dir. Default highlight = most recently modified.

7.4 Client behaviors on receive





Destination: ~/Downloads/herdr-ferry/<name>; collisions get -2, -3.



Directories (kind=dir) extracted in place into ~/Downloads/herdr-ferry/<name>/.



clipboard-image and any image/* file → set OS clipboard to the image (macOS: osascript or arboard; Linux: wl-copy/xclip).



clipboard-text → set OS clipboard to text, do not write a file.



Notification: macOS osascript -e 'display notification…' (or terminal-notifier if present, to make it clickable); Linux notify-send. Body = note or name + size.



--open flag: open/xdg-open the file after save.



Verify sha256 before ack; on mismatch, retry once, then leave claimed and log.

7.5 Herdr manifest (herdr-plugin.toml)

id = "<handle>.ferry"
name = "Ferry"
version = "0.1.0"
min_herdr_version = "0.8.0"
description = "Move files and clipboard between the Herdr box and your laptop over SSH"
platforms = ["macos", "linux"]

[[build]]
command = ["cargo", "build", "--release", "--locked"]

[[startup]]
command = ["target/release/herdr-ferry", "status-token"]

[[actions]]
id = "send"
title = "Ferry: send file to laptop"
contexts = ["pane", "workspace"]
command = ["target/release/herdr-ferry", "send-from-context"]

[[actions]]
id = "send-clipboard"
title = "Ferry: send clipboard to laptop"
contexts = ["pane", "workspace"]
command = ["target/release/herdr-ferry", "send", "--clip"]

[[actions]]
id = "paste-inbox"
title = "Ferry: paste latest inbox path into agent"
contexts = ["pane"]
command = ["target/release/herdr-ferry", "inbox-paste"]

[[panes]]
id = "picker"
title = "Ferry"
placement = "popup"
width = "70%"
height = 18
command = ["target/release/herdr-ferry", "picker"]

[[events]]
on = "pane.exited"
command = ["target/release/herdr-ferry", "status-token"]

[[link_handlers]]
id = "file-url"
title = "Ferry this file"
pattern = "^file://.+"
action = "send"

send-from-context: if selected_text in context is an existing path → send it directly; otherwise open the picker (HERDR_BIN_PATH plugin pane open --plugin <id> --entrypoint picker).

⚠ VERIFY: contexts valid values (docs show ["workspace"]; confirm "pane" is accepted or drop it). ⚠ VERIFY: whether [[events]] accepts pane.exited as a hook name — docs list it as a subscription event; link-time warnings will tell you. ⚠ VERIFY: HERDR_BIN_PATH pane send-text CLI wrapper name and flags for pane.send_text (raw socket method is confirmed; wrapper name is a guess).

Suggested user keybinding (documented in README, not shipped):

[[keys.command]]
key = "prefix+f"
type = "plugin_action"
command = "<handle>.ferry.send"
description = "ferry file to laptop"

7.6 Sidebar token

status-token calls workspace.report_metadata for every workspace with tokens = { ferry = "<n>" } where n = queued outbox items sourced from that workspace, null when zero, ttl_ms unset. Users add $ferry to their sidebar row format. Called from startup, after every send, and after outbox-ack (the client's ack runs on the server via SSH, so it can update the token too).

8. Transport details





All client→server calls go through ssh <alias> using the user's config; the plugin never stores hosts or keys. --alias defaults to HERDR_FERRY_ALIAS env, then a client.toml under ~/.config/herdr-ferry/.



File pull: ssh <alias> cat <payload> piped to a local file with progress, or scp when scp is present. Prefer ssh cat (one code path, works with tar streams, supports ControlMaster).



Remote binary discovery: ssh <alias> 'command -v herdr-ferry || echo $HOME/.herdr-ferry/bin/herdr-ferry'. README instructs adding the plugin's target/release to PATH or symlinking; doctor checks it.



Version handshake: outbox-wait prints a version header line; client warns on mismatch, refuses on major mismatch.



watch reconnect: exponential backoff 1s→30s on SSH failure; log to stderr; never exit on transient errors. Exit cleanly on SIGINT.



Encourage ControlMaster auto + ControlPersist 10m in README; doctor detects it.

9. Security & privacy





No new listeners, no new keys, no new trust: SSH only.



Server-side commands only read/write under the state dir and the paths the user explicitly selected. send refuses paths outside $HOME unless --allow-outside-home.



push into inbox never overwrites; ids are unique.



inbox-paste inserts text into a pane; it never sends Enter. (Herdr pane.send_text vs send_keys — use text only.)



Refuse to send files matching a deny-list by default (.env*, *.pem, id_*, *.key, .netrc, *credentials*) unless --force; print the reason.



Nothing is uploaded anywhere but the user's own two machines.

10. Milestones

M0 — skeleton (2–3 h). Cargo project, clap CLI, item/sidecar model, send, outbox-list, outbox-wait (fs watch via notify), pull over ssh cat, doctor. Test manually between two dirs on one machine using ssh localhost.

M1 — plugin (2–3 h). Manifest, send-from-context, status-token, link locally with herdr plugin link, bind prefix+f. Verify all ⚠ items. Ship watch with notifications + image clipboard on macOS.

M2 — picker + push (3–4 h). ratatui popup with the three candidate sources; push --paste; inbox-paste; directory tarballs; dedupe; claim-timeout recovery.

M3 — publish (1 h). README with install (herdr plugin install <handle>/herdr-ferry), client setup (cargo install --git or GitHub release binaries via cargo-dist), herdr-plugin topic, screenshots/GIF.

11. Later (explicitly not v0.1)





Direct push over tailnet: if client.toml names a reachable client host, server-side send shells ssh <client> herdr-ferry receive immediately instead of waiting for the poll. Falls back to outbox.



Windows client: clipboard via arboard, notifications via powershell/toast; ssh cat works unchanged.



Agent skill file: a short SKILL.md-style note telling agents herdr-ferry send <path> --note "…" exists.



Phone: expose history/outbox-list as JSON for herdr-remote/collie to render.



Reverse selection: pick a file on the laptop from inside Herdr (needs a client listener; conflicts with "client pulls only").

12. Acceptance criteria





From a Herdr pane on the server, prefix+f → picker → Enter puts a 5 MB PNG on the laptop clipboard and in ~/Downloads/herdr-ferry/ within 2 s (ControlMaster on) with a native notification.



herdr-ferry push bug.png --paste from the laptop results in the inbox path appearing in the focused agent's input on the server without submitting.



Killing the client mid-pull leaves the item recoverable; the next watch completes it.



Sending a directory of 300 files arrives extracted with identical sha256 tree.



doctor on a fresh laptop explains every missing prerequisite in one screen.



herdr plugin install from GitHub builds with only cargo present; no network beyond crates.io.



Sidebar shows $ferry count and it returns to blank after pull.

13. Crate shortlist

clap (derive), serde/serde_json, ulid, sha2, notify (fs watch), tar + flate2, ignore (gitignore-aware walk), regex, ratatui + crossterm (picker), arboard (clipboard, client side, optional feature), anyhow, tracing/tracing-subscriber, directories. Avoid tokio in v0.1; everything is blocking subprocess I/O.

14. Notes for Claude Code





Start by running herdr api schema --json > docs/herdr-api.schema.json and herdr --help / herdr pane --help on the server; resolve every ⚠ VERIFY against them before writing manifest code. Record findings in docs/herdr-verified.md.



Keep server-side commands free of any dependency that needs a display or clipboard; feature-gate client extras (--features client).



Every subcommand must work when invoked with the plugin env vars absent (plain shell) and present (Herdr). Test both.



Integration test harness: spawn sshd-free tests by pointing --alias at a fake ssh shim script on PATH that execs the command locally. Real SSH tested manually.



Log to HERDR_PLUGIN_STATE_DIR/ferry.log when under Herdr so herdr plugin log list and the file agree.

