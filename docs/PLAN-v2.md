# gray-comms v2 plan

Goal: every running Gray session knows what the others are doing and can message any of them, with no setup and no manual join. Ideas taken from 0xmmo/crew, built on what Gray core already has (`gray read`, `gray send`).

## What is wrong today

| Problem | Effect seen on 2026-11-13 |
|---|---|
| One mesh name per machine (config `name`) | every session is `gray-e2e`; sessions cannot be told apart |
| Presence is opt-in (`comms_join`) | no session ever joined; `comms_who` shows only test leftovers alice/bob |
| No awareness | agents do not know other agents exist or what they are doing |
| DMs land only at the next user prompt (peek in prompt/context) | a busy agent never sees a message until its turn ends and the user types |
| Everything depends on the daemon | daemon was not installed; all comms tools failed |
| Stale config | `gray_bin` pointed at a missing path; `workdir` is a deleted /tmp dir from an e2e run |
| LLM "who answers" router on every room message | cost and latency for a feature nobody uses yet |

## Design

1. **Identity = Gray session id.** Read from Gray core (env or registry). Display label: short id + model + cwd basename. Optional nickname via `comms_name`. The config `name` becomes a machine prefix for LAN use only.
2. **Presence from Gray core's live-session registry**, not from joins. Busy/idle from session activity. Works with the daemon down.
3. **Awareness block** in `prompt/context`: the other live sessions (skip self, skip idle > 30 min): short id, model, busy/idle, cwd, one-line recap (latest user ask + latest assistant summary), last 3 transcript lines. Re-sent only when changed (hash per session under `~/.gray/comms/seen/`). Hard cap ~1200 chars. Ends with one line on how to message.
4. **Messaging** `comms_send {to, text, kickstart?}`: `to` = id prefix, pid, cwd substring, nickname, `#room`, or `all`. Live sessions get it through Gray core's `gray send` path (mid-turn). Every message is also written to SQLite for history and for sessions that are not live. Sender attribution is automatic. Exactly-once delivery by message id.
5. **Claims** `comms_status "editing src/x.rs"`: a short free-text "working on" line shown in everyone's awareness block, auto-expiring after 30 min. This is what lets two agents share one checkout without stepping on each other.
6. **Daemon becomes optional**: needed only for rooms, durable history across reboots, and LAN. Local awareness and send work without it. `doctor` says which mode is active.
7. **Human view**: `gray-comms crew` and `/comms crew`: every live session with status, recap, tail, pending mail; `--json` for scripts.
8. **Routing decider**: keep the code, default off, only for `auto` rooms.

## Phases (each ends with a real end-to-end check, no unit tests)

1. Cleanup: fix config defaults, purge test agents, doctor checks `gray_bin`, live push and daemon mode. Check: `doctor` clean on this laptop.
2. Identity + presence from Gray core. Check: two throwaway `gray -p` sessions both appear in `gray-comms crew` with distinct ids and correct busy/idle.
3. Awareness block. Check: a throwaway session's injected context lists the other live sessions; it is not re-sent when nothing changed.
4. `comms_send` + kickstart via `gray send`. Check: a message sent to a busy throwaway session lands mid-turn; a kickstart makes it act; an idle target keeps it pending and shows it in `crew`.
5. Claims. Check: a claim appears in the other session's block and expires.
6. Daemon-optional mode + README/RUNTIME rewrite. Check: everything in 2-5 works with the daemon stopped.
7. Crew parity (0xmmo/crew feature set inside gray-comms): Claude Code, Codex and OpenCode sessions join the same awareness and mail through their own hooks (`gray-comms install-hook` merges into ~/.claude/settings.json, ~/.codex/hooks.json, ~/.config/opencode/plugins/, idempotent), reading their transcripts read-only for status/recap/tail; `--ttl` on messages (default 24h); `--all` broadcast; `crew` as a CLI alias of `gray-comms crew` (symlink installed by install-hook); `doctor --fix`. Check: a throwaway Claude Code `claude -p` session and a throwaway Gray session see each other and exchange one message.

Rules: cargo only on maid; no new code comments; no unit tests; explicit enums for modes; `git commit --no-verify`.

## Open questions (answered by the Gray core research)

- Where the live-session registry lives and how `gray read` finds busy/idle.
- How `gray send` lands: mid-turn after a tool call, at turn end, or next prompt; what happens to an idle session.
- Whether a plugin can see its own session id (env var) and whether `prompt/context` fires mid-turn or only per prompt.
