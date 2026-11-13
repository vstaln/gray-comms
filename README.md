# gray-comms

Cross-agent message mesh for gray: rooms, direct messages, and presence over
a tiny TCP daemon. A port of the ideas in npm `agent-comms` — reimplemented
in Rust as a central daemon instead of a P2P mesh.

Two grays on one box (or a LAN) talk to each other through it. Other
harnesses can interop on the wire — it's newline-delimited JSON.

Every Gray session also takes part automatically: it joins the mesh under its
own name, sees the other live sessions in its prompt, and can message one of
them mid-turn — no room, no registration, nothing to call.

## What it is

- **Daemon** (`gray-comms run`): a tokio TCP server holding room membership,
  presence, and a bounded durable history in SQLite. Wire ops: `join`,
  `leave`, `join_room`, `say`, `dm`, `who`, `rooms`, `peek`, `status`, `ping`.
- **Sidecar** (`gray-comms sidecar`): exposes six tools to the model —
  `comms_send`, `comms_join`, `comms_say`, `comms_dm`, `comms_who`,
  `comms_read` — plus a `prompt/context` hook that notes newly arrived DMs
  and injects the live-session block (below).
- **CLI**: `crew`, `send`, `join`, `say`, `dm`, `who`, `rooms`, `read`,
  `listen` for humans and scripts, plus
  `setup/install/status/stop/restart/doctor/uninstall`.

## Sessions, presence and awareness

- **Identity.** Each session joins the mesh as its own name: the config
  `name` is a prefix, the suffix is the Gray session's short id —
  `gray-e2e.unita`, `gray-e2e.vorti`, … `"identity": "fixed"` keeps the old
  single-name behaviour. Names are computed per call from `GRAY_SESSION_ID`
  (or the hook's `session.id`), never stored.
- **Presence.** The sidecar opens one connection for the life of the session
  and pushes `busy`/`idle` from the session's own transcript activity, so
  `who` shows running sessions without anyone calling `comms_join`.
- **Awareness block.** The `prompt/context` hook prepends a compact block of
  the *other* live sessions — short id, status, model, cwd, a cheap recap
  (latest user line and latest assistant line) and the last 3 transcript
  lines — capped at ~1200 chars, self and sessions idle >30 min excluded.
  Sessions come from Gray's own live-session registry (`~/.gray/sessions`
  plus the `.open` lock), so agents that never joined the mesh still appear.
  The rendered block is hashed per session under `~/.gray/comms/seen/`; an
  unchanged block is not injected again.

## Wire protocol

One JSON object per line.

```json
→ {"id":1,"method":"join","params":{"name":"alice","listen":true,"rooms":["dev"]}}
← {"id":1,"result":{"agent":{"id":"alice","name":"alice","status":"active"},"rooms":["dev"],"joined":["dev"],"missed":0}}
← {"method":"notify","event":{"type":"dm","message":{...}}}        (push, no id)
→ {"id":2,"method":"dm","params":{"to":"bob","text":"hi"}}
← {"id":2,"result":{"message":{...},"delivery":"delivered"}}
```

Event shapes mirror agent-comms' `DeliveryEvent` (`room_message`, `dm`,
`member_joined`, `member_left`); messages carry `id`, `from`, `to`/`room`,
`content`, `timestamp` (RFC 3339), `readBy`. Delivery semantics: a DM is
`delivered` when pushed to a live listener or read via `peek
{"unread":true}`; otherwise it waits in the daemon's SQLite queue.

## Install

```sh
cargo build --release
gray-comms setup        # name, localhost/LAN bind, port, home room, token
gray-comms install      # runit / systemd --user / detached, auto-detected
```

`gray-comms setup` (or `gray-comms register`) writes the `comms` entry in
gray's plugin lock *and* refreshes `~/.gray/plugins/comms-manifest.json`, the
manifest cache lazy sessions read — a re-register is what makes new tools
visible without a full reinstall. The entry is marked `global` so the
awareness block also reaches lean sessions. To inspect without installing:
`gray plugin check <path-to-binary>`.

## Config

`~/.config/gray-comms/config.json` (mode 0600):

| key | meaning |
|---|---|
| `name` | mesh prefix (`[A-Za-z0-9_.-]{1,64}`); the whole name under `identity: fixed` |
| `identity` | `session` (default: join as `<name>.<short id>`) or `fixed` |
| `name_prefix` | overrides `name` as the prefix in `session` mode |
| `bind` | daemon bind address — `127.0.0.1` (default) or `0.0.0.0` for LAN |
| `host` | where clients connect (default: `bind`; `0.0.0.0` → loopback) |
| `port` | TCP port (default `19876`, agent-comms' own default) |
| `home_room` | default room for `comms_say` (default `lobby`) |
| `token` | optional shared secret required at `join` — set for LAN |
| `history_limit` | room history kept per room (default 500, max 100k) |
| `routing` | optional "who answers" section — see below |
| `gray_bin` / `gray_home` / `workdir` | absolute paths, written by setup |

State (agents, rooms, memberships, message log) lives in `comms.sqlite`
beside the config and survives daemon restarts.

### Room routing ("who answers")

Off by default. When `routing.enabled` is on, a room listed in
`routing.rooms` narrows which members get the live `room_message` push —
history still records the message for everyone:

```json
"routing": {
  "enabled": true,
  "timeout_ms": 1500,
  "rooms": {"dev": "auto", "ops": "lead"},
  "leads": {"dev": "alice"},
  "descriptions": {"alice": "ops lead — deploys and incidents"},
  "model": {"base_url": "http://127.0.0.1:4000/v1", "model": "local-small",
            "api_key_env": "OPENAI_API_KEY"}
}
```

- `lead` — only the lead (`leads.<room>`, else the first member) is pushed.
- `auto` — a mention-less message triggers one cheap model call that picks
  a responder from the member list (plus an `@several` option): p ≥ 0.6
  pushes only that member, `@several` ≥ 0.6 pushes everyone, anything less
  falls back to the lead. Any error, timeout, or unparseable answer fails
  open — the room does exactly what it did before.
- `model` is an OpenAI-Responses upstream (`{base_url}/responses`); when
  absent the decider uses the cheap tier of `<gray_home>/router.json`
  (`tiers.cheap`, then `default_tier`, then cheapest configured). No
  resolvable model = fail open.
- Every call that reaches the model appends one scrubbed row to
  `routing-log/YYYY-MM.ndjson` next to `comms.sqlite` (mode 0600) — choice,
  p, latency, a 16-char state hash; never message text.

## Tools

- `comms_send {to?, text, all?, mode?}` — message another live session by
  short id, id prefix, pid or cwd substring, or every other session with
  `all:true`. Delivery is one `gray send`, so it lands mid-turn and is
  exactly-once; `mode:"kickstart"` frames the text as a directive. A target
  that is no live session falls back to a mesh DM queued in the daemon.
- `comms_join {room?, status?}` — subscribe a room (presence is automatic)
- `comms_say {room?, text}` — post to a room (default: `home_room`)
- `comms_dm {to, text}` — DM an agent; reports `delivered`/`queued`
- `comms_who {room?}` — mesh agents or room members with live presence
  (`gray-comms crew` is the live-session view instead)
- `comms_read {room?|with?|unread?|limit?}` — room history, a DM thread,
  the DM inbox, or only newly arrived DMs

## Limits

- Plaintext TCP — LAN mode is trust + one shared token, not real auth. Keep
  `bind` on `127.0.0.1` unless the LAN is trusted.
- One daemon per config file; no cross-daemon federation (agent-comms' full
  mesh protocol — capability tokens, CBOR handshake — is deliberately not
  mirrored; only the JSON-lines framing and event/message shapes are).
- Messages ≤ 16 KiB; wire lines ≤ 256 KiB; `peek` ≤ 100 per call.
- `who` presence means "a connection joined as that name is open": a session
  holds one for its lifetime, and a short-lived client shows as online only
  while it is connected.
- The block is a snapshot per turn; a busy neighbour churns it, so it is
  re-injected whenever it changes, not only when sessions come and go.

## CLI additions

```sh
gray-comms crew [--json] [--tail N] [--dir DIR] [--all]   # live sessions
gray-comms send "hold off src/x.rs" --to vorti [-k]       # message a session
gray-comms send "deploying now" --all                     # every other session
```
