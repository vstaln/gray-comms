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

## Answers from the Gray core research

- **Registry**: `<gray_home>/sessions/` — `<id>.jsonl` (header: id, timestamp, cwd, model, origin) plus `<id>.open`, live iff the lock is held (`gray/src/session_store/open_lock.rs:62-102`). `gray read --live` is the gateway plugin's `session read` (`gray/main.rs:133-150` → `gray-gateway/src/session/read.rs:36-92`): pid from the `.open` holder, cwd/model from the header line, last activity from the transcript mtime.
- **Busy/idle** is not persisted. `SessionEnd {Done, ToolUse, ToolResult, UserUnanswered}` comes from the last transcript entry (`session_store/scan.rs:87-97`) and `SessionState {Done, Interrupted, Running}` (`resume.rs:99-118`) where `Running` only means the process is alive. A peer reads busy as: live ∧ last entry not a finished assistant turn.
- **`gray send`**: a file mailbox (`<home>/cron/inbox/<id>/<ms>-<uuid>.json`, gateway `session/send.rs:66-93`) drained by the target. Interactive sessions wake every 2 s (`repl/mod.rs:958-971`), turn the entry into a prompt and start a turn; mid-turn the steer hook (`prompt_turn.rs:169-179` → `session_send.rs:171-197`) is polled before every model call (`gray-core/src/agent_loop.rs:544-560`). `-p` sessions drain only at the start of a `--resume` run (`print.rs:1188-1204`), so a message to a print session waits for its next resume.
- **Identity**: every hook/tool call carries `session {id, cwd, model?}` (`gray-plugin/src/sidecar.rs:63-69`); the sidecar process also inherits `GRAY_SESSION_ID`, `GRAY_SESSION_ORIGIN`, `GRAY_HOME`, `GRAY_MODEL`.
- **`prompt/context` fires once per turn, before the first model call** (`agent_loop.rs:513-532`), wrapped as `[Context update]` and skipped when an identical injected message already exists (`:520-530`). It cannot fire mid-turn; per-model-call alternatives are `context/build`, `tool/before`, `tool/after`.

## Deviations from this plan (as built)

- `comms_send` takes `mode: "message" | "kickstart"` instead of a boolean `kickstart`, because modes are one explicit enum here.
- A message to a busy **`-p`** session cannot land mid-turn: Gray print sessions drain mail only when resumed (core limitation above). Interactive sessions do steer mid-turn. The phase-4 check uses an interactive-style target where the behaviour is real and documents the print case.
- The awareness block is not gated by a *changed* hash alone: core already drops identical `[Context update]` text, so the plugin keeps a per-session seen-hash to stay token-frugal and core's dedupe is the second net.
- A live idle session is *not* "pending": `gray send` wakes it and the message is consumed within a second. Pending (`queued (session not live)`) only exists for sessions that are not running, so `crew` shows a `· N mail` badge on live rows and a `not live · N queued message(s)` footer for the rest (top 5 plus a count).
- The claim tool is `comms_status` as planned; the CLI verb is `gray-comms claim` because `gray-comms status` already means "is the daemon up".
- Claims live in `<gray_home>/comms/claims/<session>.json` and are filtered on read by age (30 min), so expiry needs no timer or daemon; a claim expired by age is simply ignored and never re-injected.
- `comms_send` falls back to the session store when no live session matches, so an exact id or unique prefix of a stopped session queues mail for it instead of erroring.
- A `gray -p` session is never idle while alive (it exits when its turn ends), so the phase-2 check pairs one `-p` (busy) with one `gray --stdio` (idle) throwaway instead of two `-p` sessions.
- Phase 1: `workdir` no longer defaults to the config directory (a deleted `/tmp` path broke it); setup writes `$HOME` and an explicit `identity: "session"`. The machine prefix is `vstaln`, not the e2e leftover `gray-e2e`.

## Phase checks (as run on this laptop)

**1 — cleanup.** `gray-comms doctor` all green: gray binary, `daemon mode up at 127.0.0.1:19876`, `live push` listener online, session store 9 live and matching `gray read --live`, identity `session (prefix vstaln): this session joins as vstaln.unita`, block 1091 chars, state dir, registration enabled+global. alice/bob purged from `comms.sqlite` (agents 23 → 21).

**2 — identity + presence.** Two throwaways in `/tmp/gc-throw`: `isothermal-photon-sheath` (busy, `gray -p`, pid 17426) and `asymmetric-oligomer-libration` (idle, `gray --stdio`, pid 14715). `gray-comms crew` shows `11 live session(s) · you are unitary-excimer-lattice (vstaln.unita)`, then `• isoth busy … /tmp/gc-throw · pid 17426 · 11s ago` and `• asymm idle … /tmp/gc-throw/probe · pid 14715 · 39s ago`; `crew --json` confirms `isoth busy / isothermal-photon-sheath 17426` and `asymm idle / asymmetric-oligomer-libration 14715`.

**3 — awareness block.** Real session: the throwaway `inert-protonium-supercell` (`gray -p`, cwd `/tmp/gc-throw/realdemo`) carries exactly one injected `[Context update]` in its transcript, 1340 chars: `comms: 11 other Gray session(s) live`, then `• isoth busy · claude-opus-5-5 · /tmp/gc-throw`, `• unita busy`, `• lumin busy` with a recap (`… ⇒ …`), three tail lines and the closing `Message one: comms_send …` line. Deterministic throttle with one synthetic peer and the daemon unreachable (port 19999): call 1 → BLOCK 615 chars (`• alpha idle … 20:25 › index the film shots / 20:25 ‹ shots.json parsed, 39 shots`), call 2 → static only 199 chars, then a `tool_use` appended to the peer → call 3 → BLOCK 630 chars with `• alpha busy` and a `20:25 ⚙ grep` line, call 4 → static only again.

**4 — send + kickstart via `gray send`.** Busy target `asymmetric-oligomer-libration` (`gray --stdio`, running `bash sleep 40`): `gray-comms send "mid-turn ping from the phase-four check" --to asymm` → `sent as message from vstaln.unita: asymm (asymmetric-oligomer-libration) queued id 3fc250bc`, and the target's own stream shows `{"type":"steered","text":"↳ from vstaln.unita\nmid-turn ping from the phase-four check"}` **inside** turn `turn-1a1d516aa50-8`, before its `turn_end`. Kickstart to the idle target: `sent as kickstart from vstaln.unita: asymm … delivered id eb674eee`, the target logged `↳ from vstaln.unita\nDirective — act on this now, before anything else.\nReply with exactly: kickstart-ok` as a new `turn_start` with no prompt from me and answered `kickstart-ok`. Pending: a send during a long turn reports `queued id fcc4d8b7` and `crew` shows `• asymm busy … · 1 mail · pid 14715`, which clears once the steer lands; a send to the stopped `isothermal-photon-sheath` reports `(not live) queued (session not live)` and `crew` lists `• isothermal-photon-sheath not live · 1 queued message(s) · delivered when that session resumes`.

**5 — claims.** The throwaway `radiative-yttrium-inselberg` (`gray --stdio`) was asked to call the tool and echoed `claim set for radiative-yttrium-inselberg: editing src/session.rs — other live sessions see it in their next context block; it expires in 30m`. `gray-comms crew` then showed `claim: editing src/session.rs` under that row, and the next fresh throwaway (`cosmic-bimeron-playa`, cwd `/tmp/gc-throw/claimread`) had it in its injected block: `• radia idle · claude-opus-5-5 · /tmp/gc-throw/claim` / `  claim: editing src/session.rs`. Back-dating `set_ms` (the tool's 30-minute window is the only thing that decides expiry): 29 min → `claim: editing src/session.rs (29m)` in the block, 31 min → no claim line and none in `crew`; `gray-comms claim --clear --session …` removed the file.

**6 — daemon-optional.** `gray-comms stop` (runit `sv down`, nothing listening on 19876) and then: `doctor` exits 0 with `• daemon mode — down (daemon not reachable (is `gray-comms run` up?)); local run: crew, the block and send` and every other line ✓; `crew` lists 12 live sessions; the throwaway `photonic-gravitino-aurora` got a 1360-char injected block with 11 peers; the throwaway `vortical-beryllium-aphelion` (`gray --stdio`, cwd `/tmp/gc-throw/dfa`) called `comms_send` and got `sent as message from vstaln.vortical-b: radia (radiative-yttrium-inselberg) delivered id 70050928` — the target turned that into a new turn and answered `↳ from vstaln.vortical-b / daemon-off ping from dfa`; `comms_status` set `auditing the daemon-off path`, which `crew` showed (`claim: auditing the daemon-off path (9m)`) and the next block carried at 1098 block chars. Daemon restarted afterwards. Note: one `-p` run in the middle of this (`meridional-technetium-cyclone`) never got the block even though the hook ran and wrote its seen-hash — that run was also retrying the provider (`sidecar request timed out (provider/chat)`), so it is a degraded-provider artifact, not a daemon dependency; re-runs with the daemon stopped inject normally.

**7 — crew parity.** `install-hook` merges a `SessionStart` + `UserPromptSubmit` hook into `~/.claude/settings.json` (`/home/vstaln/grayplugins/gray-comms/target/release/gray-comms hook --harness claude`, timeout 10 s), the same pair into `~/.codex/hooks.json`, an opencode plugin file into `~/.config/opencode/plugins/gray-comms.js`, and the `~/.local/bin/crew` symlink; a second run keeps one entry per event, so it is idempotent. A live Claude Code session (`claude -p --session-id 317bd687-… --input-format stream-json`, pid 5282) appears in `crew` as `• claud idle · claude · claude · /tmp/gc-throw/claude-demo · pid 5282` and in a Gray session's block as `• claud idle · claude · claude · /tmp/gc-throw/claude-demo / what did the hook say ⇒ You've hit your 5-hour limit …`. The Claude Code side got the Gray sessions in its own `SessionStart` hook output (1131 chars: `comms: 9 other session(s) live`, `• genom busy`, `• ultra busy … ⇒ Both are well along. gray-comms is about halfway through its phases`, `• igneo busy · deepseek-v4.1-flash`, `• vortical-b idle`, `… You are claud.`). One message crossed: `gray-comms send "pizzicato from the phase-seven check" --to claud` → `queued for its next hook call (1 message(s) waiting, ttl 1440m)`, and the hook then handed the harness `↳ from vstaln.unita` / `pizzicato from the phase-seven check`; the mailbox file was 0 bytes afterwards and a second hook call printed no mail (exactly-once). `doctor --fix` reports `fixed: plugin registration and manifest cache` only when the lock actually changed.

What the hooks needed: the hook's own parent pid is a short-lived `sh -c`, so liveness is a harness process running in the session's cwd (`/proc/<pid>/cwd`), deduped per pid and pruned after 30 minutes. Delivery to a harness is a local mailbox under `~/.gray/comms/foreign/`, drained exactly once per hook call, with `ttl` (default 24h).
