use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Message,
    Kickstart,
}

impl Mode {
    pub fn parse(raw: Option<&str>) -> Result<Mode, String> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("message") => Ok(Mode::Message),
            Some("kickstart") => Ok(Mode::Kickstart),
            Some(other) => Err(format!(
                "\"mode\" must be \"message\" or \"kickstart\", not \"{other}\""
            )),
        }
    }

    pub fn payload(self, text: &str) -> String {
        match self {
            Mode::Message => text.to_string(),
            Mode::Kickstart => {
                format!("Directive — act on this now, before anything else.\n{text}")
            }
        }
    }

    pub fn wait_secs(self) -> u64 {
        match self {
            Mode::Message => 2,
            Mode::Kickstart => 6,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Message => "message",
            Mode::Kickstart => "kickstart",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    One(String),
    All,
}

impl Target {
    pub fn parse(to: Option<&str>, all: bool) -> Result<Target, String> {
        match (all, to.map(str::trim).filter(|s| !s.is_empty())) {
            (true, _) => Ok(Target::All),
            (false, Some(raw)) => Ok(Target::One(raw.to_string())),
            (false, None) => {
                Err("give `to` (id prefix, pid or cwd substring) or set all:true".to_string())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Session(String),
    SessionAll(Vec<String>),
    Mesh(String),
}

impl Route {
    pub fn label(&self) -> String {
        match self {
            Route::Session(id) => format!("session {id}"),
            Route::SessionAll(ids) => format!("{} session(s)", ids.len()),
            Route::Mesh(name) => format!("mesh dm to {name}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Sent {
    pub id: String,
    pub state: String,
}

pub fn send(
    config: &serde_json::Value,
    gray_bin: &str,
    session: &str,
    from: &str,
    mode: Mode,
    text: &str,
) -> Result<Sent, String> {
    if gray_bin.trim().is_empty() {
        return Err("no gray_bin in the config".to_string());
    }
    let out = std::process::Command::new(gray_bin)
        .arg("send")
        .arg(session)
        .arg("--from")
        .arg(from)
        .arg("--wait")
        .arg(mode.wait_secs().to_string())
        .arg("--")
        .arg(mode.payload(text))
        .output()
        .map_err(|_| "cannot run `gray send`".to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        return Err(format!("`gray send` failed: {detail}"));
    }
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| format!("`gray send` printed no JSON: {}", stdout.trim()))?;
    let sent = Sent {
        id: v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        state: v
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("queued")
            .to_string(),
    };
    crate::mailog::append(config, from, session, mode.label(), &sent.state, text);
    Ok(sent)
}

pub fn steer(
    config: &serde_json::Value,
    gray_bin: &str,
    run: &str,
    from: &str,
    mode: Mode,
    text: &str,
) -> Result<Sent, String> {
    if gray_bin.trim().is_empty() {
        return Err("no gray_bin in the config".to_string());
    }
    let out = std::process::Command::new(gray_bin)
        .arg("subagents")
        .arg("steer")
        .arg(run)
        .arg("--")
        .arg(mode.payload(text))
        .env_remove("GRAY_SUBAGENTS_ACTIVE")
        .output()
        .map_err(|_| "cannot run `gray subagents steer`".to_string())?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if detail.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            detail
        };
        return Err(format!("`gray subagents steer` failed: {detail}"));
    }
    let sent = Sent {
        id: run.to_string(),
        state: "steered".to_string(),
    };
    crate::mailog::append(config, from, run, mode.label(), "steered", text);
    Ok(sent)
}
