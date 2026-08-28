# herdr-ferry

Move files and clipboard content between the machine [Herdr](https://herdr.dev) runs on and the laptop you're attached from — over the SSH you already have. No cloud bucket, no new listeners, no new keys.

- **Send** — inside Herdr on the server, hit a key, pick the file the agent just produced, and it lands in `~/Downloads/herdr-ferry/` on the laptop within a second, with a notification. Images go straight onto the laptop clipboard.
- **Push** — from the laptop, `herdr-ferry push bug.png --paste` drops the file on the server and types its path into the focused agent's input (not submitted).
- **Agent-usable** — an agent in a pane can run `herdr-ferry send report.pdf --note "Q3 summary"` and you get it.

One Rust binary, runs on both sides. The laptop *pulls*; the server never needs to reach the laptop, so it works over plain SSH to a NAT'd box, Tailscale, or a jump host.

```
  LAPTOP                                           SERVER (runs Herdr + this plugin)
  herdr-ferry watch ──ssh alias──▶ outbox-wait     ~/.herdr-ferry/outbox/<id>/
        ▲                            (long-poll)   plugin actions: send · send-clipboard · paste-inbox
        └── file / clipboard / toast               picker popup · $ferry sidebar token
  herdr-ferry push  ──ssh alias──▶ inbox-receive   ~/.herdr-ferry/inbox/<id>/  → pane send-text
```

## Install

### Server (the Herdr box)

```sh
herdr plugin install wavrin/herdr-ferry      # clones, runs `cargo build --release --locked`
```

Needs `cargo` on the server. Then put the binary on `PATH` so the laptop can find it over SSH:

```sh
ln -s "$(herdr plugin list --json | python3 -c 'import json,sys;print([p for p in json.load(sys.stdin)["result"]["plugins"] if p["plugin_id"]=="herdr-ferry"][0]["plugin_root"])')/target/release/herdr-ferry" ~/.local/bin/herdr-ferry
```

Bind a key in `~/.config/herdr/config.toml` (pick one that's free — `prefix+f` is often taken by herdr-file-viewer):

```toml
[[keys.command]]
key = "prefix+y"
type = "plugin_action"
command = "herdr-ferry.send"
description = "ferry file to laptop"
```

Optionally show the queue count in the sidebar:

```toml
[ui.sidebar.spaces]
rows = [["state_icon", "workspace"], ["branch", "git_status", "$ferry"]]
```

### Client (the laptop)

```sh
cargo install --git https://github.com/wavrin/herdr-ferry     # or grab a release binary
mkdir -p ~/.config/herdr-ferry
cat > ~/.config/herdr-ferry/client.toml <<EOF
alias = "alfred"            # your ~/.ssh/config Host alias
# dest = "~/Downloads/herdr-ferry"
# remote_bin = "/path/to/herdr-ferry"   # if not on the server's PATH
EOF
herdr-ferry doctor          # explains anything missing
herdr-ferry watch           # leave running (a terminal tab, tmux, or a launchd agent)
```

Strongly recommended in `~/.ssh/config` so each poll reuses one connection:

```
Host alfred
  ControlMaster auto
  ControlPath ~/.ssh/cm-%r@%h:%p
  ControlPersist 10m
```

## Use

| Where | What | How |
|---|---|---|
| Herdr, any pane | send a file | `prefix+y` → picker (fuzzy filter, `Tab` multi-select, `Enter`) — or select a path in the pane first and the key sends it directly |
| Herdr | send server clipboard | action `herdr-ferry.send-clipboard` |
| Herdr | modifier-click a `file://` link | link handler "Ferry this file" |
| Server shell / agent | `herdr-ferry send ./out/dash.png --note "look"` | queues it; `send DIR` tars it |
| Server shell | `herdr-ferry send --clip` | server clipboard (image or text) |
| Laptop | `herdr-ferry watch` / `pull` | deliver continuously / once |
| Laptop | `herdr-ferry push shot.png --paste` | to server inbox; path typed into the focused agent |
| Either | `herdr-ferry history` · `doctor` | |

On receive: files → `~/Downloads/herdr-ferry/<name>` (collisions get `-2`, `-3`); directories extracted in place; `image/*` also copied to the clipboard; clipboard-text items go to the clipboard only. `--open` opens each file after saving.

## Safety

- SSH only. Nothing leaves your two machines; the plugin stores no hosts or keys.
- `send` refuses paths outside `$HOME` (`--allow-outside-home`) and names matching `.env*`, `*.pem`, `id_*`, `*.key`, `.netrc`, `*credentials*` (`--force`).
- `push` never overwrites; every item gets its own ULID directory.
- `paste-inbox` inserts text only — it never presses Enter.
- Integrity: sha256 checked before ack; a client killed mid-pull leaves the item claimed, and it reverts to queued after 10 minutes so the next `watch` finishes it.

## State

`$HERDR_PLUGIN_STATE_DIR` under Herdr, else `~/.herdr-ferry/`:

```
outbox/<ulid>/{payload | <name>.tar.gz, item.json}
inbox/<ulid>/{<name>, item.json}
history.jsonl
ferry.log
```

## Development

```sh
cargo build --release
herdr plugin link "$PWD"                 # then check: herdr plugin list --json → warnings
herdr plugin action invoke herdr-ferry.send
```

Client commands can be tested without a real server by putting a fake `ssh` on `PATH` that runs its last argument locally. `docs/herdr-verified.md` records the Herdr 0.8.2 facts the manifest relies on.

## License

MIT
