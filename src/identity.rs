use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    Session { prefix: Option<String> },
    Fixed(String),
}

fn sanitize(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
            out.push(ch);
        } else {
            out.push('-');
        }
        if out.chars().count() >= 64 {
            break;
        }
    }
    out
}

impl Identity {
    pub fn from_config(config: &Value) -> Result<Identity, String> {
        let name = config
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        match config
            .get("identity")
            .and_then(Value::as_str)
            .unwrap_or("session")
        {
            "fixed" => {
                if name.is_empty() {
                    return Err("identity \"fixed\" needs a name in the config".to_string());
                }
                Ok(Identity::Fixed(sanitize(&name)))
            }
            "session" => {
                let prefix = config
                    .get("name_prefix")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| (!name.is_empty()).then_some(name));
                Ok(Identity::Session { prefix })
            }
            other => Err(format!(
                "unknown identity \"{other}\"; use \"session\" or \"fixed\""
            )),
        }
    }

    pub fn mesh_name(&self, short: &str) -> String {
        match self {
            Identity::Fixed(name) => name.clone(),
            Identity::Session { prefix } => match prefix {
                Some(p) => {
                    let p = sanitize(p);
                    let room = 64usize.saturating_sub(p.chars().count() + 1).max(1);
                    format!("{p}.{}", short.chars().take(room).collect::<String>())
                }
                None => sanitize(short),
            },
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Identity::Session { .. } => "session",
            Identity::Fixed(_) => "fixed",
        }
    }
}

pub fn session_id(params: &Value) -> Option<String> {
    if let Some(id) = params
        .pointer("/session/id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(id.to_string());
    }
    std::env::var("GRAY_SESSION_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn mesh_name_now(config: &Value, session_id: Option<&str>) -> Result<String, String> {
    let ident = Identity::from_config(config)?;
    Ok(match session_id {
        Some(id) => ident.mesh_name(&crate::session::short_for(config, id)),
        None => match &ident {
            Identity::Fixed(name) => name.clone(),
            Identity::Session { prefix } => prefix.clone().unwrap_or_else(|| "gray".to_string()),
        },
    })
}

pub fn session_model(params: &Value) -> Option<String> {
    params
        .pointer("/session/model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}
