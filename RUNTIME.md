# Runtime

What runs, what it reads, and what happens when a piece is missing.

## Processes

| process | who starts it | lifetime | needs the daemon |
|---|---|---|---|
| `gray-comms run --config ~/.config/gray-comms/config.json` | the supervisor: runit `~/.config/service/gray-comms`, or systemd `--user`, or a detached spawn with a pidfile | until stopped | is the daemon |
| one sidecar per live Gray session | Gray, when it first needs the plugin (`gray comms` with no subcommand on a pipe) | the session's | no |
| `gray-comms <verb>` | you, or a script | one shot | only for room/DM verbs |

The sidecar keeps one thread for presence: it joins the mesh as the
session's own name and pushes `busy`/`idle` every 10 s. Without a daemon
that thread retries every 15 s in the background and changes nothing else.

## Hook

`prompt/context` fires once per agent run, before the first model call. The
sidecar answers with the static line, any newly arrived DMs (only if the
daemon answers), and — when its hash changed since this session last saw it —
the awareness block. Gray wraps the answer in `[Context update]` and skips a
block it already has in the conversation.

## State

| path | written by | shape |
|---|---|---|
| `~/.config/gray-comms/config.json` | `setup` | name, identity, bind/port, home room, `gray_bin`, `gray_home`, `workdir` |
| `~/.config/gray-comms/comms.sqlite` | the daemon | agents, rooms, memberships, message history |
| `~/.gray/comms/seen/<session>.json` | the sidecar | last block hash and length per session |
| `~/.gray/comms/claims/<session>.json` | `comms_status` | claim text, `set_ms`, TTL |
| `~/.gray/sessions/*.jsonl`, `*.open` | Gray | the live-session registry, read-only here |
| `~/.gray/cron/inbox/<session>/` | `gray send` | queued messages, only counted here |
| `~/.gray/plugins/lock.json`, `~/.gray/plugins/comms-manifest.json` | `gray-comms register` | the plugin entry (`global: true`) and the manifest cache |

Nothing in `~/.gray/comms/` is required for correctness: delete `seen/` and
the next prompt re-injects the block, delete `claims/` and the claims are
gone.

## Degraded modes

- **No daemon** — block, `crew`, `comms_send`, `comms_status` and pending
  mail keep working; `comms_join/say/dm/who/read` return a fixable error and
  `doctor` shows `• daemon mode — down` with an exit code of 0.
- **No live peers** — the hook returns the static line only; no block, no
  re-injection state is written.
- **No `gray` binary** — `comms_send` reports that `gray_bin` is unusable and
  `doctor` fails that check; everything read-only still works.
- **Target session gone** — `comms_send` queues an exact-id match in the
  target's `gray send` inbox, which it drains on its next run.
