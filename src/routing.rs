//! "Who answers" room routing, modeled on OpenMausBot's decision model
//! (docs/decision-model.md). When a mention-less message lands in a room
//! whose mode is `auto`, one fast cheap model call picks the responder out
//! of the member list. The decision only narrows the live `room_message`
//! push — history is stored for everyone either way — and every failure
//! fails open: the room does exactly what it did without routing (every
//! live member is pushed).
//!
//! No HTTP crate is linked: the call goes through `curl` with a hard
//! `--max-time` budget and the request body on stdin so message text never
//! shows in `ps`. The upstream defaults to the cheap tier of gray's
//! provider table (`<gray_home>/router.json` → `tiers.cheap`, then
//! `default_tier`, then the cheapest configured); `routing.model` in the
//! comms config overrides it.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Reserved answer meaning "several members should respond". `@` is not a
/// legal agent-name char, so it can never collide with a real member id.
pub const SEVERAL: &str = "@several";
/// Confidence at or above which a model pick narrows the responder set.
pub const P_MIN: f64 = 0.6;
/// Default decision budget, matching the documented ~1.5s.
const DEFAULT_TIMEOUT_MS: u64 = 1500;
/// Message text is clipped before going into the decider prompt.
const MAX_TEXT_CHARS: usize = 4000;

/// Which members get the live `room_message` push.
#[derive(Debug, PartialEq, Eq)]
pub enum Targets {
    /// Every member (today's behavior — also the fail-open outcome).
    All,
    /// Only these member names.
    Only(Vec<String>),
}

/// A failed model call; `reason` is the scrubbed token that lands in the log.
pub struct Fail {
    pub reason: &'static str,
    pub status: Option<u16>,
}

impl Fail {
    fn new(reason: &'static str) -> Self {
        Fail {
            reason,
            status: None,
        }
    }
}

/// One upstream endpoint, OpenAI-Responses flavored (`{base_url}/responses`).
/// Same shape as a gray-router tier.
#[derive(Clone, Debug)]
pub struct ModelSpec {
    pub base_url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
}

impl ModelSpec {
    fn from_value(v: &Value) -> Option<Self> {
        let base_url = v.get("base_url").and_then(Value::as_str)?.trim().to_string();
        let model = v.get("model").and_then(Value::as_str)?.trim().to_string();
        if base_url.is_empty() || model.is_empty() {
            return None;
        }
        Some(ModelSpec {
            base_url,
            model,
            api_key_env: v
                .get("api_key_env")
                .and_then(Value::as_str)
                .map(str::to_string),
            api_key: v
                .get("api_key")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    fn url(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    /// Bearer for this upstream: env first, then the literal (router's order).
    fn api_key(&self) -> Option<String> {
        if let Some(var) = &self.api_key_env {
            if let Ok(v) = std::env::var(var) {
                if !v.trim().is_empty() {
                    return Some(v);
                }
            }
        }
        self.api_key.clone().filter(|k| !k.trim().is_empty())
    }
}

/// Parsed `routing` section of the comms config. All semantics live here so
/// the daemon's `say` path is one `route()` call.
pub struct Routing {
    /// Master switch. Off (or a missing `routing` section) = every room
    /// answers `All`, exactly as before routing existed.
    enabled: bool,
    /// Hard budget for the model call; clamped 200ms..=30s.
    timeout: Duration,
    /// room → "auto" | "lead". Rooms not listed behave as today.
    rooms: BTreeMap<String, String>,
    /// room → lead agent name (the fallback responder).
    leads: BTreeMap<String, String>,
    /// agent → free-text "title — description" offered to the model.
    descriptions: BTreeMap<String, String>,
    /// `routing.model` override; absent → gray provider config.
    model: Option<ModelSpec>,
    gray_home: Option<PathBuf>,
}

impl Routing {
    pub fn from_config(config: &Value) -> Self {
        let empty = json!({});
        let r = config.get("routing").unwrap_or(&empty);
        let str_map = |key: &str| -> BTreeMap<String, String> {
            r.get(key)
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        };
        let gray_home = config
            .get("gray_home")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("GRAY_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".gray")));
        Routing {
            enabled: r
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            timeout: Duration::from_millis(
                r.get("timeout_ms")
                    .and_then(Value::as_u64)
                    .map(|n| n.clamp(200, 30_000))
                    .unwrap_or(DEFAULT_TIMEOUT_MS),
            ),
            rooms: str_map("rooms")
                .into_iter()
                .filter(|(_, v)| v == "auto" || v == "lead")
                .collect(),
            leads: str_map("leads"),
            descriptions: str_map("descriptions"),
            model: ModelSpec::from_value(r.get("model").unwrap_or(&Value::Null)),
            gray_home,
        }
    }

    /// Who gets the live push for this room message. `members` is the full
    /// member list (sender included); `log_dir` gets one scrubbed ndjson row
    /// per model call, `None` skips logging (tests, memory stores).
    pub fn route(
        &self,
        room: &str,
        from: &str,
        text: &str,
        members: &[String],
        log_dir: Option<&Path>,
    ) -> Targets {
        self.route_with(room, from, text, members, log_dir, &curl_call)
    }

    fn route_with(
        &self,
        room: &str,
        from: &str,
        text: &str,
        members: &[String],
        log_dir: Option<&Path>,
        call: &dyn Fn(&ModelSpec, &str, Duration) -> Result<String, Fail>,
    ) -> Targets {
        if !self.enabled {
            return Targets::All;
        }
        match self.rooms.get(room).map(String::as_str) {
            Some("lead") => self
                .lead(room, members)
                .map(|l| Targets::Only(vec![l]))
                .unwrap_or(Targets::All),
            Some("auto") => self.decide(room, from, text, members, log_dir, call),
            _ => Targets::All,
        }
    }

    /// The room's fallback responder: configured lead if still a member,
    /// else the first member.
    fn lead(&self, room: &str, members: &[String]) -> Option<String> {
        if let Some(l) = self.leads.get(room) {
            if members.iter().any(|m| m == l) {
                return Some(l.clone());
            }
        }
        members.first().cloned()
    }

    /// `auto` mode: ask the model who should answer. Any failure — no model
    /// configured, timeout, transport, HTTP error, or an answer that does
    /// not check out — fails open to `All`.
    fn decide(
        &self,
        room: &str,
        from: &str,
        text: &str,
        members: &[String],
        log_dir: Option<&Path>,
        call: &dyn Fn(&ModelSpec, &str, Duration) -> Result<String, Fail>,
    ) -> Targets {
        let offered: Vec<String> = members
            .iter()
            .filter(|m| m.as_str() != from)
            .cloned()
            .collect();
        // An empty pick list or an @-addressed message needs no decision.
        if offered.is_empty() || mentions_member(text, members) {
            return Targets::All;
        }
        let Some(spec) = self.model_spec() else {
            return Targets::All;
        };
        let clipped: String = text.chars().take(MAX_TEXT_CHARS).collect();
        let state = json!({
            "room": room,
            "from": from,
            "members": offered,
            "text": clipped,
        });
        let mut row = json!({
            "at": crate::store::iso(crate::store::now_secs()),
            "seam": "roomRouting",
            "room": room,
            "model": spec.model,
            "stateHash": state_hash(&state),
        });
        let prompt = self.prompt(room, from, &clipped, &offered);
        let started = Instant::now();
        let res = block_in_place(|| call(&spec, &prompt, self.timeout));
        row["latencyMs"] = json!(started.elapsed().as_millis() as u64);
        let targets = match res {
            Err(f) => {
                row["ok"] = json!(false);
                row["reason"] = json!(f.reason);
                if let Some(s) = f.status {
                    row["status"] = json!(s);
                }
                Targets::All
            }
            Ok(raw) => match parse_pick(&raw, &offered) {
                Err(reason) => {
                    row["ok"] = json!(false);
                    row["reason"] = json!(reason);
                    Targets::All
                }
                Ok((choice, p)) => {
                    row["ok"] = json!(true);
                    row["choice"] = json!(choice);
                    row["p"] = json!(p);
                    if p >= P_MIN {
                        if choice == SEVERAL {
                            Targets::All
                        } else {
                            Targets::Only(vec![choice])
                        }
                    } else {
                        match self.lead(room, members) {
                            Some(l) => Targets::Only(vec![l]),
                            None => Targets::All,
                        }
                    }
                }
            },
        };
        if let Some(dir) = log_dir {
            append_row(dir, &row);
        }
        targets
    }

    /// The decider prompt: members with optional "title — description"
    /// lines, the `@several` option, the new message, and the strict
    /// reply contract.
    fn prompt(&self, room: &str, from: &str, text: &str, offered: &[String]) -> String {
        let mut list = String::new();
        for m in offered {
            list.push_str("- ");
            list.push_str(m);
            if let Some(d) = self.descriptions.get(m) {
                if !d.trim().is_empty() {
                    list.push_str(" — ");
                    list.push_str(d.trim());
                }
            }
            list.push('\n');
        }
        list.push_str("- ");
        list.push_str(SEVERAL);
        list.push_str(" — several members should answer\n");
        format!(
            "Pick which member of the chat room \"#{room}\" should answer the new message.\n\
             {list}\n\
             Message from {from}:\n{text}\n\n\
             Reply with a JSON object only, no prose:\n\
             {{\"choice\": \"<member name or {SEVERAL}>\", \"p\": <confidence 0.0-1.0>}}"
        )
    }

    /// The upstream to call: `routing.model` wins; else the cheap tier of
    /// gray's provider table (`<gray_home>/router.json`). `None` = the
    /// decider has nothing to call, so every room fails open.
    fn model_spec(&self) -> Option<ModelSpec> {
        if let Some(m) = &self.model {
            return Some(m.clone());
        }
        let path = self.gray_home.as_ref()?.join("router.json");
        let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        let tiers = v.get("tiers")?.as_object()?;
        // Cheap tier default, then the user's default, then cheapest first.
        let mut pref: Vec<String> = vec!["cheap".to_string()];
        if let Some(d) = v.get("default_tier").and_then(Value::as_str) {
            pref.push(d.to_string());
        }
        pref.extend(["fast", "smart"].iter().map(|s| s.to_string()));
        pref.extend(tiers.keys().cloned());
        for name in pref {
            if let Some(m) = tiers.get(&name).and_then(ModelSpec::from_value) {
                return Some(m);
            }
        }
        None
    }
}

/// True when `text` @-addresses any member — already routed, no decider.
fn mentions_member(text: &str, members: &[String]) -> bool {
    members.iter().any(|m| {
        let pat = format!("@{m}");
        let mut rest = text;
        while let Some(i) = rest.find(&pat) {
            let after = &rest[i + pat.len()..];
            let ended = after
                .chars()
                .next()
                .map(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
                .unwrap_or(true);
            if ended {
                return true;
            }
            rest = after;
        }
        false
    })
}

/// Validate a raw model reply into `(choice, p)`. Answers that do not
/// check out — a choice never offered, a probability that is not a number
/// or outside 0..=1 — are rejected so the caller fails open.
fn parse_pick(raw: &str, offered: &[String]) -> Result<(String, f64), &'static str> {
    let s = raw.trim();
    let (a, b) = match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b > a => (a, b),
        _ => return Err("bad_answer"),
    };
    let v: Value = serde_json::from_str(&s[a..=b]).map_err(|_| "bad_answer")?;
    let choice = v
        .get("choice")
        .or_else(|| v.get("pick"))
        .and_then(Value::as_str)
        .ok_or("bad_answer")?;
    if choice != SEVERAL && !offered.iter().any(|m| m == choice) {
        return Err("invalid_choice");
    }
    let p = v
        .get("p")
        .or_else(|| v.get("probability"))
        .or_else(|| v.get("confidence"))
        .and_then(|p| p.as_f64().or_else(|| p.as_str().and_then(|s| s.parse().ok())))
        .ok_or("bad_answer")?;
    if !p.is_finite() || !(0.0..=1.0).contains(&p) {
        return Err("invalid_p");
    }
    Ok((choice.to_string(), p))
}

/// 16-hex-char fingerprint of the routing state — enough to spot repeats
/// without ever writing message text to the log.
fn state_hash(state: &Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    state.to_string().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Pull the model's text out of a Responses (or chat-completions) reply.
fn output_text(v: &Value) -> Option<String> {
    if let Some(t) = v.get("output_text").and_then(Value::as_str) {
        return Some(t.to_string());
    }
    if let Some(items) = v.get("output").and_then(Value::as_array) {
        let mut out = String::new();
        for item in items {
            for c in item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(t) = c.get("text").and_then(Value::as_str) {
                    out.push_str(t);
                }
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    v.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Run a blocking call without stalling the async worker when a
/// multi-thread runtime is live (the daemon's). Outside tokio — unit
/// tests — it just runs inline.
fn block_in_place<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// POST a tiny OpenAI-Responses request through `curl`. The body goes on
/// stdin so message text never appears in the process list; `timeout` is
/// a hard total budget (`--max-time`). Any failure maps to a scrubbed
/// `Fail` the caller turns into the open outcome.
fn curl_call(spec: &ModelSpec, prompt: &str, timeout: Duration) -> Result<String, Fail> {
    let body = json!({
        "model": spec.model,
        "store": false,
        "max_output_tokens": 200,
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": prompt}],
        }],
    });
    let mut cmd = std::process::Command::new("curl");
    cmd.arg("-sS")
        .arg("--max-time")
        .arg(format!("{:.3}", timeout.as_secs_f64()))
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("--data-binary")
        .arg("@-")
        .arg("-w")
        .arg("\n%{http_code}")
        .arg(spec.url())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    if let Some(key) = spec.api_key() {
        cmd.arg("-H")
            .arg(format!("Authorization: Bearer {key}"));
    }
    let mut child = cmd.spawn().map_err(|_| Fail::new("unavailable"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.to_string().as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|_| Fail::new("transport"))?;
    if !out.status.success() {
        let reason = if out.status.code() == Some(28) {
            "timeout"
        } else {
            "transport"
        };
        return Err(Fail::new(reason));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body_text, status) = match text.rfind('\n') {
        Some(i) => (&text[..i], text[i + 1..].trim().parse::<u16>().ok()),
        None => (text.as_ref(), None),
    };
    if !status.is_some_and(|s| (200..300).contains(&s)) {
        return Err(Fail {
            reason: "http",
            status,
        });
    }
    let resp: Value = serde_json::from_str(body_text).map_err(|_| Fail {
        reason: "bad_answer",
        status,
    })?;
    output_text(&resp).ok_or(Fail {
        reason: "bad_answer",
        status,
    })
}

/// Append one scrubbed row to `<dir>/YYYY-MM.ndjson` (dir 0700, file 0600).
/// Logging must never break routing: every error is swallowed.
fn append_row(dir: &Path, row: &Value) {
    let name = chrono::Utc::now().format("%Y-%m").to_string();
    let path = dir.join(format!("{name}.ndjson"));
    let Ok(mut line) = serde_json::to_string(row) else {
        return;
    };
    line.push('\n');
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    if b.create(dir).is_err() {
        return;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let Ok(mut f) = opts.open(&path) else {
        return;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    let _ = f.write_all(line.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(rooms: Value) -> Routing {
        Routing::from_config(&json!({
            "routing": {
                "enabled": true,
                "rooms": rooms,
                "leads": {"dev": "bea"},
                "model": {"base_url": "http://127.0.0.1:1", "model": "fake"}
            }
        }))
    }

    fn members() -> Vec<String> {
        vec!["ann".into(), "bea".into(), "carol".into()]
    }

    fn fake(body: &str) -> impl Fn(&ModelSpec, &str, Duration) -> Result<String, Fail> + '_ {
        move |_, _, _| Ok(body.to_string())
    }

    fn failing(
        reason: &'static str,
    ) -> impl Fn(&ModelSpec, &str, Duration) -> Result<String, Fail> {
        move |_, _, _| Err(Fail::new(reason))
    }

    #[test]
    fn disabled_is_today() {
        let r = Routing::from_config(&json!({}));
        assert_eq!(r.route("dev", "ann", "hi", &members(), None), Targets::All);
    }

    #[test]
    fn unlisted_room_is_today() {
        let r = cfg(json!({"dev": "auto"}));
        assert_eq!(r.route("ops", "ann", "hi", &members(), None), Targets::All);
    }

    #[test]
    fn mention_skips_the_decider() {
        let r = cfg(json!({"dev": "auto"}));
        assert_eq!(
            r.route("dev", "ann", "@bea can you look?", &members(), None),
            Targets::All
        );
        // `@be` must not count as a mention of `bea`.
        let t = r.route_with(
            "dev",
            "ann",
            "@beast check",
            &members(),
            None,
            &fake(r#"{"choice":"bea","p":0.9}"#),
        );
        assert_eq!(t, Targets::Only(vec!["bea".into()]));
    }

    #[test]
    fn lead_mode_pushes_only_the_lead() {
        let r = cfg(json!({"dev": "lead"}));
        assert_eq!(
            r.route("dev", "ann", "hi", &members(), None),
            Targets::Only(vec!["bea".into()])
        );
        // No configured lead → first member.
        let r = cfg(json!({"ops": "lead"}));
        assert_eq!(
            r.route("ops", "ann", "hi", &members(), None),
            Targets::Only(vec!["ann".into()])
        );
    }

    #[test]
    fn confident_single_member_responds() {
        let r = cfg(json!({"dev": "auto"}));
        let t = r.route_with(
            "dev",
            "ann",
            "deploy is red",
            &members(),
            None,
            &fake(r#"{"choice":"carol","p":0.91}"#),
        );
        assert_eq!(t, Targets::Only(vec!["carol".into()]));
    }

    #[test]
    fn confident_several_means_everyone() {
        let r = cfg(json!({"dev": "auto"}));
        for body in [
            r#"{"choice":"@several","p":0.8}"#,
            r#"{"choice":"@several","p":1.0}"#,
        ] {
            assert_eq!(
                r.route_with("dev", "ann", "heads up", &members(), None, &fake(body)),
                Targets::All
            );
        }
    }

    #[test]
    fn threshold_boundary() {
        let r = cfg(json!({"dev": "auto"}));
        // Exactly 0.6 narrows.
        let t = r.route_with(
            "dev",
            "ann",
            "x",
            &members(),
            None,
            &fake(r#"{"choice":"bea","p":0.6}"#),
        );
        assert_eq!(t, Targets::Only(vec!["bea".into()]));
        // 0.59 falls back to the lead (bea is configured lead of dev).
        let t = r.route_with(
            "dev",
            "ann",
            "x",
            &members(),
            None,
            &fake(r#"{"choice":"carol","p":0.59}"#),
        );
        assert_eq!(t, Targets::Only(vec!["bea".into()]));
        // No configured lead → first member.
        let r = cfg(json!({"ops": "auto"}));
        let t = r.route_with(
            "ops",
            "ann",
            "x",
            &members(),
            None,
            &fake(r#"{"choice":"carol","p":0.3}"#),
        );
        assert_eq!(t, Targets::Only(vec!["ann".into()]));
    }

    #[test]
    fn invalid_answers_fail_open() {
        let r = cfg(json!({"dev": "auto"}));
        for body in [
            "not json",
            r#"{"choice":"nobody","p":0.9}"#,   // never offered
            r#"{"choice":"ann","p":0.9}"#,     // sender not offered
            r#"{"choice":"bea"}"#,             // p missing
            r#"{"choice":"bea","p":"high"}"#,  // p not a number
            r#"{"choice":"bea","p":1.4}"#,     // p out of range
            r#"{"choice":"bea","p":null}"#,
            r#"{}"#,
            "",
        ] {
            assert_eq!(
                r.route_with("dev", "ann", "hi", &members(), None, &fake(body)),
                Targets::All,
                "body: {body}"
            );
        }
    }

    #[test]
    fn call_errors_fail_open() {
        let r = cfg(json!({"dev": "auto"}));
        for reason in ["timeout", "transport", "http", "unavailable", "bad_answer"] {
            assert_eq!(
                r.route_with("dev", "ann", "hi", &members(), None, &failing(reason)),
                Targets::All
            );
        }
    }

    #[test]
    fn no_model_configured_fails_open() {
        // Enabled + auto room but no `routing.model` and no router.json.
        let r = Routing::from_config(&json!({
            "routing": {"enabled": true, "rooms": {"dev": "auto"}},
            "gray_home": "/nonexistent-gray-home"
        }));
        assert_eq!(r.route("dev", "ann", "hi", &members(), None), Targets::All);
    }

    #[test]
    fn fenced_and_prose_wrapped_answers_parse() {
        let r = cfg(json!({"dev": "auto"}));
        let body = "Here you go:\n```json\n{\"choice\": \"bea\", \"p\": 0.7}\n```\nDone.";
        assert_eq!(
            r.route_with("dev", "ann", "hi", &members(), None, &fake(body)),
            Targets::Only(vec!["bea".into()])
        );
    }

    #[test]
    fn every_call_writes_one_scrubbed_row() {
        let dir = tempfile::tempdir().unwrap();
        let r = cfg(json!({"dev": "auto"}));
        let t = r.route_with(
            "dev",
            "ann",
            "super secret message text",
            &members(),
            Some(dir.path()),
            &fake(r#"{"choice":"bea","p":0.9}"#),
        );
        assert_eq!(t, Targets::Only(vec!["bea".into()]));
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(files.len(), 1);
        let path = files[0].as_ref().unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let row: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(row["seam"], "roomRouting");
        assert_eq!(row["ok"], true);
        assert_eq!(row["choice"], "bea");
        assert!(!text.contains("super secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
