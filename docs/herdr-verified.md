# Herdr facts verified for herdr-ferry

Verified 2026-08-28 against the installed binary (`herdr 0.8.2`, API protocol 20,
schema in `docs/herdr-api.schema.json`), https://herdr.dev/docs/plugins/,
https://herdr.dev/docs/marketplace/ and https://herdr.dev/docs/socket-api/.
Resolves every ⚠ VERIFY in spec.md.

## Marketplace listing (https://herdr.dev/plugins/)

- Public GitHub repo + GitHub topic **`herdr-plugin`**. That's it — no submission.
- `herdr-plugin.toml` at repo root (subdirs also allowed, monorepo-style).
- Index refreshes every 30 min and rescans when the default-branch head moves.
  Forks, archived repos, and unparseable manifests are excluded.
- Card shows: repo name/owner/description, stars, language, last push, plus one row per
  manifest with `name`, `version` and a link to the source dir. So the **GitHub repo
  description** is what users read on the card — set it.
- Install command shown = `herdr plugin install <owner>/<repo>`.
- Marketplace-tracked manifest fields: `id`, `name`, `version`, `platforms`, `min_herdr_version`.

## Manifest schema (from `InstalledPluginInfo` / `PluginManifest*` in the API schema)

Top level: `id`, `name`, `version`, `min_herdr_version` (required); `description`,
`platforms` (`linux|macos|windows`) optional.

| Section | Fields |
|---|---|
| `[[build]]` | `command` (argv), `platforms?` |
| `[[startup]]` | `command`, `platforms?` — env `HERDR_PLUGIN_EVENT=startup` |
| `[[actions]]` | `id`, `title`, `command` (required); `contexts`, `description`, `platforms?` |
| `[[events]]` | `on` (string), `command`, `platforms?` |
| `[[panes]]` | `id`, `title`, `command` (required); `placement` (default `overlay`), `width`, `height`, `description`, `platforms?` |
| `[[link_handlers]]` | `id`, `title`, `pattern`, `action` (required); `platforms?` |

- **`contexts` enum: `global`, `workspace`, `tab`, `pane`, `selection`.** ✅ `"pane"` is valid.
  `selection` is also useful for the send action (fires when text is selected).
- **`placement` enum: `overlay`, `popup`, `split`, `tab`, `zoomed`.** ✅
- **`width`/`height` (`PopupSize`)**: integer cells (outer size incl. border) or `"NN%"` string
  matching `^(100|[1-9][0-9]?)%$`. ✅ `width = "70%"`, `height = 18` are both valid.
- Duplicate action ids are rejected at load time even if platform-gated (herdr-file-viewer
  manifest comment, verified on 0.7.1).
- Unknown event names produce **non-fatal warnings** surfaced by `herdr plugin list`
  (`warnings[]`). So after `herdr plugin link .`, run `herdr plugin list --json` and check
  `warnings` to confirm `pane.exited` is accepted as a hook.
- Subscription event names that exist (schema): `pane.created`, `pane.closed`, `pane.focused`,
  `pane.moved`, `pane.exited`, `pane.updated`, `pane.agent_status_changed`, `pane.agent_detected`,
  `pane.scroll_changed`, `pane.output_matched`, `workspace.created/closed/focused/moved/renamed/
  reordered/updated/metadata_updated`, `tab.created/closed/focused/moved/renamed`. Docs example
  uses `worktree.created` in `[[events]]`.

## Env vars passed to plugin commands

`HERDR_SOCKET_PATH`, `HERDR_BIN_PATH`, `HERDR_ENV=1`, `HERDR_PLUGIN_ID`, `HERDR_PLUGIN_ROOT`,
`HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_STATE_DIR`, `HERDR_PLUGIN_CONTEXT_JSON`,
`HERDR_PLUGIN_ACTION_ID` (actions), `HERDR_PLUGIN_EVENT` / `HERDR_PLUGIN_EVENT_JSON` (startup +
event hooks), `HERDR_PLUGIN_ENTRYPOINT_ID` (panes), `HERDR_PLUGIN_CLICKED_URL` /
`HERDR_PLUGIN_LINK_HANDLER_ID` (link handlers), `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`,
`HERDR_PANE_ID` (when available).

## `HERDR_PLUGIN_CONTEXT_JSON` shape (`PluginInvocationContext`)

All fields nullable:
`workspace_id`, `workspace_label`, `workspace_cwd`, `tab_id`, `tab_label`,
`focused_pane_id`, **`focused_pane_cwd`**, `focused_pane_agent`, `focused_pane_status`,
**`selected_text`**, `clicked_url`, `link_handler_id`, `invocation_source`, `correlation_id`,
`worktree`.

✅ Resolves spec §7.2: use `focused_pane_cwd` directly; fall back to
`herdr pane get <id>` → `PaneInfo.foreground_cwd` / `cwd` when null.
`source.agent` in item.json ← `focused_pane_agent`.

## CLI wrappers (exact, from `herdr pane` / `herdr workspace` / `herdr plugin` usage)

```
herdr pane send-text <pane_id> <text>                       # pane.send_text — text only, no Enter ✅
herdr pane send-keys <pane_id> <key> [key ...]              # do NOT use for inbox-paste
herdr pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi]
herdr pane get <pane_id>                                    # JSON PaneInfo (cwd, foreground_cwd, agent, workspace_id, tokens)
herdr pane current [--pane ID|--current]
herdr pane list [--workspace ID]
herdr workspace list
herdr workspace report-metadata <workspace_id> --source ID [--token NAME=VALUE] [--clear-token NAME] [--seq N] [--ttl-ms N]
herdr plugin pane open --plugin ID --entrypoint ID [--placement ...] [--width SIZE] [--height SIZE] [--workspace ID] [--target-pane PANE] [--cwd PATH] [--env K=V] [--focus|--no-focus]
herdr plugin pane close <pane_id>
herdr plugin action invoke <action_id> [--plugin ID]
herdr plugin list [--plugin ID] [--json]
herdr plugin log list [--plugin ID] [--limit N]
herdr plugin link <path> [--disabled]  /  unlink / enable / disable / install / uninstall
herdr api schema [--json | --output PATH]
```

Note: `herdr <sub> --help` prints the top-level help; run `herdr pane` (no args) for usage.
`herdr plugin` is not listed in top-level `--help` but exists.

## workspace.report_metadata (sidebar `$ferry` token)

Params: `workspace_id`, `source` (required — use `"ferry"`), `tokens` (≤16 keys, names
`^[A-Za-z0-9_-]{1,32}$`, value string or **null to clear**), `seq?`, `ttl_ms?` (1..86400000).
CLI: `herdr workspace report-metadata w1 --source ferry --token ferry=2` /
`--clear-token ferry`. Users render it via
`[ui.sidebar.spaces] rows = [["state_icon","workspace"],["branch","git_status","$ferry"]]`.
(Pane-level tokens also exist: `herdr pane report-metadata … --token`.)

## Popups

`popup` placement is session-modal; the pane process should exit (or call `popup.close`) when done.
`plugin.pane.open` accepts `env` — useful to pass the picker its context.

## Keybinding syntax (user config)

```toml
[[keys.command]]
key = "prefix+f"
type = "plugin_action"
command = "<plugin-id>.<action-id>"
description = "ferry file to laptop"
```
⚠ On this machine `prefix+f` and `prefix+shift+f` are already bound to herdr-file-viewer
(`~/.config/herdr/config.toml`). Suggest a different default in the README (e.g. `prefix+y`).

## Real-world reference

`~/.config/herdr/plugins/github/herdr-file-viewer-c993314e2614/herdr-plugin.toml` — a Rust
plugin with cargo build, `[[panes]]` with relative `./target/release/...` command, and platform
gating. Plugin root is the cwd for commands on macOS/Linux (relative commands work); Windows
cannot spawn relative pane commands (irrelevant for us — Windows excluded).
