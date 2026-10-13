# gray-comms runtime

## Processes

- `gray-comms run` — the daemon. Foreground; supervised in production.
- `gray-comms sidecar` — per gray session, spawned by the plugin host with
  argv `[binary, "sidecar", "--config", <path>]`. One NDJSON request per
  stdin line, one reply per stdout line; `plugin/shutdown`/EOF exits.
- CLI verbs (`say`, `dm`, `who`, `read`, `join`, `rooms`, `listen`) connect
  to the daemon, run the op, and exit — each is a short-lived client.

## Supervision

`gray-comms install` picks what the box actually has, like gray's own
gateway service:

- **runit** — `$SVDIR`, else `~/.config/service`, else `/var/service`;
  writes `gray-comms/run` (`#!/bin/sh exec ...`), waits for runsvdir to
  adopt it.
- **systemd --user** — `~/.config/systemd/user/gray-comms.service`,
  `enable --now`. Survives logout only with `loginctl enable-linger`.
- **none** — detached spawn with `gray-comms.pid` + `gray-comms.log` in the
  config dir; `stop` SIGTERMs the pidfile, `restart` = stop + spawn.

`status`, `stop`, `restart`, `uninstall` map to the detected supervisor.

## Files

- `~/.config/gray-comms/config.json` — config, mode 0600.
- `~/.config/gray-comms/comms.sqlite` — agents, rooms, members, messages.
- `~/.config/gray-comms/gray-comms.{pid,log}` — gray-supervised mode only.
- `~/.config/gray-comms/routing-log/YYYY-MM.ndjson` — one scrubbed row per
  "who answers" decision call (0600), only while `routing` is enabled.

## Wire

See README.md. TL;DR: NDJSON, `{"id",method,params}` → `{"id",result}` or
`{"id",error}`; live listeners also get `{"method":"notify","event":{…}}`.
Every method except `join`/`ping` requires the connection to have joined.

## DM delivery

`delivered` means the content reached a recipient: pushed to a live
`listen:true` connection, or returned by that agent's `peek {"unread":true}`
(the prompt/context hook's path). Queued DMs survive daemon restarts; a
`join listen` drains them as `dm` events.
