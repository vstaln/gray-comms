use ratatui::style::Color;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Window,
    Surface,
    Raised,
    Input,
    OnSelection,
    Faint,
    Dim,
    Muted,
    Body,
    Bright,
    Accent,
    AccentSoft,
    Success,
    Error,
    Rose,
    ToolAccent,
    ToolCommand,
    ToolDim,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Palette {
    window: Color,
    surface: Color,
    raised: Color,
    input: Color,
    on_selection: Color,
    faint: Color,
    dim: Color,
    muted: Color,
    body: Color,
    bright: Color,
    accent: Color,
    accent_soft: Color,
    success: Color,
    error: Color,
    rose: Color,
    tool_accent: Color,
    tool_command: Color,
    tool_dim: Color,
}

pub const DEFAULT: Palette = Palette {
    window: Color::Reset,
    surface: Color::Rgb(22, 22, 22),
    raised: Color::Rgb(28, 28, 28),
    input: Color::Rgb(32, 32, 32),
    on_selection: Color::Black,
    faint: Color::Rgb(70, 70, 70),
    dim: Color::Rgb(110, 110, 110),
    muted: Color::Rgb(140, 140, 140),
    body: Color::Rgb(225, 225, 225),
    bright: Color::Rgb(240, 240, 240),
    accent: Color::Rgb(246, 173, 126),
    accent_soft: Color::Rgb(254, 215, 170),
    success: Color::Rgb(74, 222, 128),
    error: Color::Rgb(239, 68, 68),
    rose: Color::Rgb(254, 205, 211),
    tool_accent: Color::Rgb(158, 206, 106),
    tool_command: Color::Rgb(224, 175, 104),
    tool_dim: Color::Rgb(108, 108, 108),
};

impl Palette {
    pub fn get(self, role: Role) -> Color {
        match role {
            Role::Window => self.window,
            Role::Surface => self.surface,
            Role::Raised => self.raised,
            Role::Input => self.input,
            Role::OnSelection => self.on_selection,
            Role::Faint => self.faint,
            Role::Dim => self.dim,
            Role::Muted => self.muted,
            Role::Body => self.body,
            Role::Bright => self.bright,
            Role::Accent => self.accent,
            Role::AccentSoft => self.accent_soft,
            Role::Success => self.success,
            Role::Error => self.error,
            Role::Rose => self.rose,
            Role::ToolAccent => self.tool_accent,
            Role::ToolCommand => self.tool_command,
            Role::ToolDim => self.tool_dim,
        }
    }

    fn role_mut(&mut self, role: &str) -> Option<&mut Color> {
        Some(match role {
            "window_bg" => &mut self.window,
            "surface_bg" => &mut self.surface,
            "raised_bg" => &mut self.raised,
            "input_bg" => &mut self.input,
            "on_selection" => &mut self.on_selection,
            "text_faint" => &mut self.faint,
            "text_dim" => &mut self.dim,
            "text_muted" => &mut self.muted,
            "text_body" => &mut self.body,
            "text_bright" => &mut self.bright,
            "accent" => &mut self.accent,
            "accent_soft" => &mut self.accent_soft,
            "success" => &mut self.success,
            "error" => &mut self.error,
            "rose" => &mut self.rose,
            "tool_accent" => &mut self.tool_accent,
            "tool_command" => &mut self.tool_command,
            "tool_dim" => &mut self.tool_dim,
            _ => return None,
        })
    }

    pub fn load(config: &Value) -> Palette {
        if let Ok(raw) = std::env::var("GRAY_MODAL_THEME") {
            let mut p = DEFAULT;
            let mut any = false;
            for pair in raw.split(',') {
                let Some((role, value)) = pair.split_once('=') else {
                    continue;
                };
                let Some(color) = parse_color(value.trim(), &Value::Null) else {
                    continue;
                };
                if let Some(slot) = p.role_mut(role.trim()) {
                    *slot = color;
                    any = true;
                }
            }
            if any {
                return p;
            }
        }
        match saved_name(config) {
            Some(name) => load_named(config, &name),
            None => DEFAULT,
        }
    }
}

fn gray_home(config: &Value) -> PathBuf {
    crate::session::home(config)
}

fn saved_name(config: &Value) -> Option<String> {
    let env = std::env::var("GRAY_THEME").ok();
    let name = env
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let home = gray_home(config);
            let text = std::fs::read_to_string(home.join("config.json")).ok()?;
            let v: Value = serde_json::from_str(&text).ok()?;
            v.get("theme")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })?;
    let builtin = name.eq_ignore_ascii_case("gray") || name.eq_ignore_ascii_case("default");
    (!builtin).then_some(name)
}

fn load_named(config: &Value, name: &str) -> Palette {
    let path = gray_home(config)
        .join("themes")
        .join(format!("{name}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return DEFAULT;
    };
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return DEFAULT;
    };
    apply(DEFAULT, &root)
}

fn apply(mut palette: Palette, root: &Value) -> Palette {
    let empty = serde_json::Map::new();
    let vars = root
        .get("vars")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let Some(colors) = root.get("colors").and_then(Value::as_object) else {
        return palette;
    };
    for (role, value) in colors {
        let Some(color) = parse_color_value(value, vars) else {
            continue;
        };
        if let Some(slot) = palette.role_mut(role) {
            *slot = color;
        }
    }
    palette
}

fn parse_color_value(value: &Value, vars: &serde_json::Map<String, Value>) -> Option<Color> {
    let raw = value.as_str()?.trim();
    if let Some(v) = vars.get(raw) {
        return parse_color(v.as_str()?.trim(), &Value::Null);
    }
    parse_color(raw, &Value::Null)
}

fn parse_color(raw: &str, _vars: &Value) -> Option<Color> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match raw.to_ascii_lowercase().as_str() {
        "reset" | "none" | "transparent" | "default" => return Some(Color::Reset),
        "black" => return Some(Color::Black),
        "white" => return Some(Color::White),
        _ => {}
    }
    let hex = raw.strip_prefix('#')?;
    let expand = |s: &str| u8::from_str_radix(s, 16).ok();
    match hex.len() {
        3 => {
            let d: Vec<u8> = hex
                .chars()
                .map(|c| {
                    let doubled: String = format!("{c}{c}");
                    u8::from_str_radix(&doubled, 16).unwrap_or_default()
                })
                .collect();
            (d.len() == 3).then(|| Color::Rgb(d[0], d[1], d[2]))
        }
        6 => {
            let r = expand(&hex[0..2])?;
            let g = expand(&hex[2..4])?;
            let b = expand(&hex[4..6])?;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

pub fn theme_path(config: &Value) -> Option<PathBuf> {
    let name = saved_name(config)?;
    Some(
        gray_home(config)
            .join("themes")
            .join(format!("{name}.json")),
    )
}
