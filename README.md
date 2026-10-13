# gray-comms

Cross-agent message mesh for gray: rooms, direct messages, and presence over
a tiny TCP daemon. A port of the ideas in npm `agent-comms` — reimplemented
in Rust as a central daemon instead of a P2P mesh.

Two grays on one box (or a LAN) talk to each other through it. Other
harnesses can interop on the wire — it's newline-delimited JSON.

## What it is

- **Daemon** (`gray-comms run`): a tokio TCP server holding room membership,
  presence, and a bounded durable history in SQLite. Wire ops: `join`,
  `leave`, `join_room`, `say`, `dm`, `who`, `rooms`, `peek`, `status`, `ping`.
- **Sidecar** (`gray-comms sidecar`): exposes five tools to the model —
  `comms_join`, `comms_say`, `comms_dm`, `comms_who`, `comms_read` — plus a
  `prompt/context` hook that notes newly arrived DMs.
- **CLI**: `join`, `say`, `dm`, `who`, `rooms`, `read`, `listen` for humans
  and scripts, plus `setup/install/status/stop/restart/doctor/uninstall`.

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

`gray-comms setup` registers the `comms_*` tools in gray's plugin lock. To
inspect without installing: `gray plugin check <path-to-binary>`.

## Config

`~/.config/gray-comms/config.json` (mode 0600):

| key | meaning |
|---|---|
| `name` | this agent's mesh name (`[A-Za-z0-9_.-]{1,64}`) |
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

- `comms_join {room?, status?}` — announce presence, optionally join a room
- `comms_say {room?, text}` — post to a room (default: `home_room`)
- `comms_dm {to, text}` — DM an agent; reports `delivered`/`queued`
- `comms_who {room?}` — agents or room members with live presence
- `comms_read {room?|with?|unread?|limit?}` — room history, a DM thread,
  the DM inbox, or only newly arrived DMs

## Limits

- Plaintext TCP — LAN mode is trust + one shared token, not real auth. Keep
  `bind` on `127.0.0.1` unless the LAN is trusted.
- One daemon per config file; no cross-daemon federation (agent-comms' full
  mesh protocol — capability tokens, CBOR handshake — is deliberately not
  mirrored; only the JSON-lines framing and event/message shapes are).
- Messages ≤ 16 KiB; wire lines ≤ 256 KiB; `peek` ≤ 100 per call.
- `who` presence is push-listener presence: a connected-but-not-listening
  client reads as `offline`.
