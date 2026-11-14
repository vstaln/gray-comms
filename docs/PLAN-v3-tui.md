# gray-comms v3 plan — the interactive TUI

Goal: typing `/comms` inside any Gray session swaps Gray's screen for a live
control room of the whole machine — every live session grouped by folder,
its transcript tail on the right, a compose line that messages it, plus
mail, rename, open and filter. `q`/`esc` puts Gray's screen back.

Nothing about delivery changes. The TUI is a second front end over the same
`session::collect`, `deliver::send` and claim files that `crew` and
`comms_send` already use.

## Screen

```
 comms · 8 live · you are ultra                                       21:58
┌ sessions ─────────────────────────┬ muoni · film director ──────────────────┐
│ ~/content                         │ opus 5.5 · busy 2h14m · ctx 161k/200k   │
│ ● muoni   film director    busy   │                                         │
│   └ ● vorti  act II/IV builder    │ › is inkling back                       │
│       ⚑ editing acts/act2.py      │ ‹ The Napoleon, Titan ...               │
│ ● ultra   3D head (you're here)   │ ⚙ ssh maid 'ls renders/act3'            │
│   └ ● refra  head sculpt          │ ⟲ exit 0 · 14 lines                     │
│ ~/gray                            │                     (live, auto-scroll) │
│ ● lumin   room builder     busy   │                                         │
│ ⚠ monso   telegram fix  retrying  │                                         │
│ other apps                        │                                         │
│ ○ claud   Claude Code      idle   │                                         │
├───────────────────────────────────┴─────────────────────────────────────────┤
│ to muoni › _                                     ⏎ send · ctrl+⏎ make it act │
└ ↑↓ pick · ⏎ type · m agent mail · n rename · o open · / filter · esc back ──┘
```

- **Live.** One redraw per second driven by the event poll timeout, never a
  thread. `terminal.draw` diffs the buffer, so there is no flicker and no
  clear-screen on refresh.
- **Left.** Folder header rows (`~` paths, not selectable), sessions under
  them, subagents nested one level under the session that spawned them.
  Foreign harness sessions (Claude Code / Codex / OpenCode) live under
  `other apps`. Busy `●`, idle `○`, warning `⚠`, own session `(you're here)`.
  The short id stays next to the name, dimmed.
- **Name.** Nickname (`n`), else Gray's session title (`~/.gray/sessions/<id>.title`),
  else a 2–4 word summary derived from the latest real user prompt. No model
  call anywhere in this path.
- **Status.** `busy` / `idle` from the registry; a `⚠ <reason>` badge when
  the transcript tail shows a provider retry loop, when a session claims busy
  but has written no transcript entry for over ten minutes (`quiet 12m`), or
  when its context is over 90% of the window. A claim prints as one `⚑` line
  under its row.
- **Right.** Header `model · busy 2h14m · ctx 161k/200k`, then the transcript
  tail (`›` asked, `‹` said, `⚙` tool, `⟲` result), auto-scrolled to the
  bottom, `PgUp`/`PgDn` to look back.

## Modes

One enum, matched exhaustively — no mode flags scattered through the code:

```rust
enum Mode { List, Compose, Mail, Rename, Filter }
```

| key | from | effect |
|---|---|---|
| `↑` `↓` | List | move the selection over selectable rows only |
| `⏎` | List | Compose, with the caret in the input line |
| `⏎` | Compose | send as a message (`deliver::send`, mid-turn) |
| `ctrl+⏎` / `alt+⏎` | Compose | send as a kickstart |
| `esc` | Compose, Mail, Rename, Filter | back to List |
| `m` | List | Mail view |
| `n` | List | Rename the selected session |
| `o` | List | resume that session in a new terminal window, detached |
| `/` | List | Filter by text (id, name, cwd, model) |
| `PgUp` `PgDn` | List, Mail | scroll the right pane / the log |
| `q`, `esc` (List) | anywhere | quit back to Gray |

A send result (`delivered` / `queued` / the error) becomes a status line that
stays for three seconds, then the footer returns.

## Nicknames

`~/.gray/comms/nicknames.json`, mode 0600, `{"<session id>": "<nick>"}`.
Written by `n` in the TUI and by `gray-comms nick <target> <name>` /
`--clear`. Resolution order in `comms_send`/the TUI: nickname (live, then
known), then the existing id-prefix / pid / cwd-substring match.

## Mail log

`~/.gray/comms/mail.ndjson` (0600), one JSON object per line —
`{"at", "from", "to", "mode", "state", "text"}` — appended by every send
that goes out through the sidecar/TUI path, whatever the transport
(live session, foreign mailbox, mesh DM). The Mail view reads the last 200.
It is history, not state: deleting it loses the log and nothing else.

## Queued mail

The TUI and `crew` list queued inbox mail (`~/.gray/cron/inbox/<id>`) only
when it is younger than 24 h. Older files are Gray's own history and are
never read for the count, never deleted.

## How it hooks into Gray

`command/run` may answer `{"terminal": [args]}`: Gray releases its TUI,
runs the plugin binary with those argv in the terminal, and restores its
screen when the child exits (`~/gray/crates/gray-plugin/src/sidecar.rs`
`command_outcome`, then `~/gray/crates/gray/src/repl/dispatch.rs` →
`maintenance::with_terminal` + `child.status()`).

| `/comms` invocation | outcome |
|---|---|
| `/comms` | `{"terminal": ["tui"]}` |
| `/comms @vorti text…` | send, `{"text": …}`, no TUI |
| `/comms !vorti text…` | kickstart, `{"text": …}`, no TUI |
| `/comms crew/send/claim/…` | today's behaviour, unchanged |

`gray-comms tui` is also what plain `crew` runs when stdout is a terminal;
`crew --plain`, a pipe, or any other subcommand keeps today's text output.

### Does the calling session's agent turn keep running while the TUI is open?

No, and it cannot: the handoff is synchronous. `dispatch.rs` wraps the child
in `with_terminal` (raw mode off, Gray's TUI released for the child) and
waits on `child.status()`, so the REPL — and therefore the turn — is blocked
for the whole time the TUI owns the screen. The other half of the question:
a `/comms` typed *while* a turn is running never reaches that path at all —
`key_watcher.rs` matches `CommandOutcome::Terminal { .. }` and pushes the
command back onto `queued_inputs`, so the turn finishes first and the TUI
opens after it. Net effect the user sees: the TUI always opens on an idle
caller, and messages sent from it arrive at *other* sessions mid-turn as
usual.

## Theme

`GRAY_MODAL_THEME` (Gray exports `role=#rrggbb,…` for child UIs) is read
first; without it, the same config Gray's `theme.rs` reads —
`GRAY_THEME`, else `theme` in `~/.gray/config.json`, else the built-in
palette — resolved from `~/.gray/themes/<name>.json` with its `vars`
indirection. Roles used: `surface_bg`, `raised_bg`, `input_bg`,
`on_selection`, `text_faint`, `text_dim`, `text_muted`, `text_body`,
`text_bright`, `accent`, `accent_soft`, `success`, `error`, `rose`,
`tool_accent`, `tool_command`, `tool_dim`.

## Files

| path | what |
|---|---|
| `src/tui/mod.rs` | loop: terminal setup, poll, draw |
| `src/tui/app.rs` | state, `Mode`, key handling, the 1 s refresh |
| `src/tui/render.rs` | the three panes |
| `src/tui/theme.rs` | `GRAY_MODAL_THEME` / theme-file palette |
| `src/nick.rs` | nickname store and resolution |
| `src/mailog.rs` | append-only mail log |

`Cargo.toml` gains `ratatui 0.29` and `crossterm 0.28` — the versions
Gray already locks.

## Verify

1. Built on maid (`cargo build --release`), binary copied back.
2. Three throwaway `gray` sessions in two `/tmp` folders: one `-p` busy, one
   `--stdio` idle, one spawning a subagent; killed afterwards.
3. `tmux new -d -s ct -x 160 -y 45 'gray-comms tui'`, captured with
   `tmux capture-pane -p -e`: list + tail, compose with a sent result, the
   mail view, a fake stale-busy session for the warning badge.
4. `/comms` typed inside a throwaway Gray TUI session in tmux: the handoff
   opens the TUI and `q` returns to Gray's screen.
5. `crew`, `crew --plain`, `send`, `claim`, `@ID`/`!ID` from `/comms` still
   behave.

## What shipped, and where it deviates

Commits: the plan doc, `feat: interactive TUI behind /comms`, the list/self
marker/keyboard fix, and the README/RUNTIME docs with `docs/captures/`.

Deviations, all deliberate:

- **`ctrl+⏎` is not distinguishable** in tmux, kitty or anything else that
  does not speak the kitty keyboard protocol, and xterm-style terminals send
  the same byte as `⏎`. The TUI asks for keyboard enhancement on start
  (`DISAMBIGUATE_ESCAPE_CODES`) and accepts `Enter+CONTROL`, so a capable
  terminal works as specified — but the portable kickstart is **`alt+⏎`**,
  which is what the footer and the README advertises.
- **`o` was verified with a stub `$TERMINAL`** that logged its argv instead
  of opening a window, so testing did not pop a window on the desktop. The
  spawn itself (`gray resume <id>`, cwd of the session, detached) is real.
- **Warnings are heuristics, not status from Gray**: a retry badge means a
  line matching a provider retry pattern in the transcript tail, a quiet
  badge means a busy session with nothing new for 10 min, and `ctx NN%` is
  only shown between 90% and 200% of the model's window — some providers
  report cumulative token counts (Gemini reported 901%), and a badge you
  cannot trust is worse than none.
- **Context window comes from `~/.gray/models.json`** per model, falling
  back to `context_window` in the config, then 200k. Sessions that never
  wrote a `usage` line show `ctx ?`.
- **Subagent parents** are read from `~/.gray/subagents/runs/*.json`
  (`child_session` → `session_id`), because a session's own jsonl carries no
  parent link. 1103 files / 18 MB is too much per second, so the full scan
  happens once and the newest 64 are re-read every 10 s. Grandchildren are
  flattened to depth 1.
- **Folder groups** are ordered `other apps` last, then the folder that
  holds me, then freshest first; a subagent row is anchored to its parent's
  folder.
- **`esc`** clears an active filter first and quits on the next press; `q`
  always quits. The footer says `q quit` so it is not a trap.
- **Sends run on a worker thread** so the refresh keeps ticking; the footer
  shows `sending to <id>…` and then the result for three seconds.
- **Mail view** reads the last 300 rows of `~/.gray/comms/mail.ndjson`
  (the file is append-only and unbounded by design).
- **`nicknames.json` is global** (`~/.gray/comms/`), shared by the TUI and
  `gray-comms nick`; `send --to <nickname>` resolves through it, so a
  nickname is also a target.
- **Nothing else was added to the inbox path**: `crew` and the TUI count
  only `~/.gray/cron/inbox` files younger than 24 h and never delete.

Not done: no live theme reload (the palette is read once at start), no
paging in the mail view, no `/comms` help text.
