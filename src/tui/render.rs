use crate::session::{Glyph, View};
use crate::tui::app::{short_dur, App, Badge, Mode, Row, Tone};
use crate::tui::theme::Role;
use chrono::TimeZone;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

const LEFT_W: u16 = 48;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let bg = Style::default().bg(app.palette.get(Role::Surface));
    f.render_widget(Block::default().style(bg), area);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(f, app, chunks[0]);
    match app.mode {
        Mode::Mail => draw_mail(f, app, chunks[1]),
        Mode::List | Mode::Compose | Mode::Rename | Mode::Filter => {
            let left = LEFT_W.min(chunks[1].width.saturating_sub(40));
            let panes = Layout::horizontal([Constraint::Length(left), Constraint::Fill(1)])
                .split(chunks[1]);
            draw_sessions(f, app, panes[0]);
            draw_detail(f, app, panes[1]);
        }
    }
    draw_input(f, app, chunks[2]);
    draw_footer(f, app, chunks[3]);
}

fn dim(app: &App) -> Style {
    Style::default().fg(app.palette.get(Role::Dim))
}

fn faint(app: &App) -> Style {
    Style::default().fg(app.palette.get(Role::Faint))
}

fn body(app: &App) -> Style {
    Style::default().fg(app.palette.get(Role::Body))
}

fn pane(app: &App, title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(faint(app))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(app.palette.get(Role::Muted)),
        ))
        .style(Style::default().bg(app.palette.get(Role::Surface)))
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let clock = chrono::Local::now().format("%H:%M").to_string();
    let me_short = app
        .views
        .iter()
        .find(|v| v.id == app.me)
        .map(|v| v.short.clone())
        .or_else(|| (!app.me.is_empty()).then(|| app.me.chars().take(5).collect()))
        .unwrap_or_default();
    let left = Line::from(vec![
        Span::styled(
            format!(" comms · {} live · you are ", app.views.len()),
            dim(app),
        ),
        Span::styled(me_short, Style::default().fg(app.palette.get(Role::Accent))),
        Span::styled(
            match app.filter.is_empty() {
                true => String::new(),
                false => format!(" · filter “{}”", app.filter),
            },
            faint(app),
        ),
    ]);
    f.render_widget(Paragraph::new(left), area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!("{clock} "), faint(app))))
            .alignment(Alignment::Right),
        area,
    );
}

fn indent_for(depth: u8) -> String {
    match depth {
        0 => " ".to_string(),
        d => format!("{}└ ", " ".repeat(2 * d as usize + 1)),
    }
}

fn fit(text: &str, width: usize) -> String {
    let width = width.max(1);
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width - 1).collect();
    out.push('…');
    out
}

fn draw_sessions(f: &mut Frame, app: &App, area: Rect) {
    let block = pane(app, "sessions");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (lines, selected) = list_lines(app, inner.width);
    let height = inner.height as usize;
    let offset = match selected {
        Some(sel) if sel + 1 > height => sel + 1 - height,
        _ => 0,
    };
    let visible: Vec<Line> = lines.into_iter().skip(offset).take(height).collect();
    f.render_widget(Paragraph::new(visible), inner);
}

fn list_lines(app: &App, width: u16) -> (Vec<Line<'static>>, Option<usize>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = None;
    let mut n = 0;
    let w = width as usize;
    for row in &app.rows {
        match row {
            Row::Folder(name) => {
                lines.push(Line::from(Span::styled(
                    format!(" {name}"),
                    Style::default().fg(app.palette.get(Role::Faint)),
                )));
            }
            Row::Session { view, depth } => {
                let v = &app.views[*view];
                let here = n == app.sel;
                if here {
                    selected_line = Some(lines.len());
                }
                for line in session_lines(app, v, *depth, w, here) {
                    lines.push(line);
                }
                n += 1;
            }
        }
    }
    (lines, selected_line)
}

fn session_lines(app: &App, v: &View, depth: u8, width: usize, here: bool) -> Vec<Line<'static>> {
    const ID_W: usize = 6;
    const STATUS_W: usize = 9;
    let badge: Badge = app.badge(v);
    let indent = indent_for(depth);
    let used = indent.chars().count() + 2 + ID_W + 1 + STATUS_W + 1;
    let room = width.saturating_sub(used);
    let name = app.label(v);
    let name = match v.me {
        true => format!("{} (you're here)", fit(&name, room.saturating_sub(14))),
        false => name,
    };
    let label = fit(&name, room);
    let id = fit(&v.short, ID_W);
    let id_field = format!(
        "{id}{}",
        " ".repeat(ID_W.saturating_sub(id.chars().count()))
    );
    let left = indent.chars().count() + 2 + ID_W + 1 + label.chars().count();
    let word = fit(&badge.text, STATUS_W);
    let pad = width.saturating_sub(left + word.chars().count() + 1);
    let status_style = match badge.tone {
        Tone::Warn => Style::default().fg(app.palette.get(Role::AccentSoft)),
        _ => Style::default().fg(app.palette.get(Role::Dim)),
    };
    let mut spans = vec![
        Span::styled(indent.clone(), faint(app)),
        Span::styled(
            format!("{} ", badge.glyph),
            Style::default().fg(app.tone_color(badge.tone)),
        ),
        Span::styled(id_field, Style::default().fg(app.palette.get(Role::Dim))),
        Span::styled(" ", faint(app)),
        Span::styled(
            label,
            match here {
                true => Style::default()
                    .fg(app.palette.get(Role::Bright))
                    .add_modifier(Modifier::BOLD),
                false => body(app),
            },
        ),
        Span::styled(" ".repeat(pad), Style::default()),
        Span::styled(word, status_style),
        Span::styled(" ", Style::default()),
    ];
    let mut line = Line::from(std::mem::take(&mut spans));
    if here {
        line = line.style(Style::default().bg(app.palette.get(Role::Raised)));
    }
    let mut out = vec![line];
    if !v.claim.is_empty() {
        let pad = " ".repeat(indent.chars().count() + 2);
        let room = width.saturating_sub(indent.chars().count() + 4);
        out.push(Line::from(vec![
            Span::styled(
                format!("{pad}⚑ "),
                Style::default().fg(app.palette.get(Role::Rose)),
            ),
            Span::styled(
                fit(&v.claim, room),
                Style::default().fg(app.palette.get(Role::Muted)),
            ),
        ]));
    }
    out
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(v) = app.selected() else {
        let block = pane(app, "nobody");
        f.render_widget(block, area);
        return;
    };
    let title = format!("{} · {}", v.short, app.label(v));
    let block = pane(app, &title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let stamp = match v.status {
        crate::session::Status::Busy => {
            let age = if v.started_ms > 0 {
                ((crate::mailog::now_ms() - v.started_ms).max(0) / 1000) as u64
            } else {
                v.idle_secs
            };
            format!("busy {}", short_dur(age))
        }
        _ => format!("idle {}", short_dur(v.idle_secs)),
    };
    let ctx = match v.ctx_used {
        0 => "ctx ?".to_string(),
        n => format!("ctx {}/{}", kfmt(n), kfmt(app.window_for(v))),
    };
    lines.push(Line::from(vec![
        Span::styled(
            v.short_model(),
            Style::default().fg(app.palette.get(Role::AccentSoft)),
        ),
        Span::styled(format!(" · {stamp} · {ctx}"), dim(app)),
    ]));
    lines.push(Line::from(Span::styled(
        match v.harness() {
            Some(h) => format!("{} · {} · {}", v.cwd_short(), h, v.id),
            None => format!("{} · {}", v.cwd_short(), v.id),
        },
        faint(app),
    )));
    if !v.claim.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("⚑ {}", v.claim),
            Style::default().fg(app.palette.get(Role::Rose)),
        )));
    }
    lines.push(Line::from(""));
    let mut tail: Vec<Line<'static>> = Vec::new();
    for l in &v.tail {
        let (glyph, style) = tail_style(app, l.glyph);
        for (i, piece) in wrap(&l.text, w.saturating_sub(2)).into_iter().enumerate() {
            let head = if i == 0 {
                format!("{glyph} ")
            } else {
                "  ".to_string()
            };
            tail.push(Line::from(vec![
                Span::styled(head, style),
                Span::styled(piece, style),
            ]));
        }
    }
    lines.extend(autoscrolled(
        tail,
        inner.height as usize - 5,
        app.scroll_back,
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

fn tail_style(app: &App, glyph: Glyph) -> (char, Style) {
    match glyph {
        Glyph::Asked => (
            glyph.ch(),
            Style::default().fg(app.palette.get(Role::Accent)),
        ),
        Glyph::Said => (glyph.ch(), body(app)),
        Glyph::Tool => (
            glyph.ch(),
            Style::default().fg(app.palette.get(Role::ToolCommand)),
        ),
        Glyph::Result => (glyph.ch(), faint(app)),
    }
}

fn autoscrolled(tail: Vec<Line<'static>>, height: usize, back: usize) -> Vec<Line<'static>> {
    let total = tail.len();
    if height == 0 || total == 0 {
        return Vec::new();
    }
    let skip = total.saturating_sub(height + back.min(total.saturating_sub(height)));
    tail.into_iter().skip(skip).take(height).collect()
}

fn draw_mail(f: &mut Frame, app: &App, area: Rect) {
    let block = pane(app, &format!("agent mail · {} messages", app.mail.len()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for e in &app.mail {
        let clock = chrono::Local
            .timestamp_millis_opt(e.at)
            .single()
            .map(|t| t.format("%H:%M").to_string())
            .unwrap_or_else(|| "--:--".to_string());
        let head = format!("{clock} {} → {} ", short_from(&e.from), short_from(&e.to));
        let mut pieces = wrap(
            &e.text.replace('\n', " "),
            w.saturating_sub(head.chars().count()),
        );
        if pieces.is_empty() {
            pieces.push(String::new());
        }
        for (i, piece) in pieces.into_iter().enumerate() {
            let style = match e.mode.as_str() {
                "kickstart" => Style::default().fg(app.palette.get(Role::Accent)),
                _ => dim(app),
            };
            let prefix = if i == 0 {
                Span::styled(head.clone(), style)
            } else {
                Span::styled(" ".repeat(head.chars().count()), Style::default())
            };
            lines.push(Line::from(vec![prefix, Span::styled(piece, body(app))]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no agent mail yet — send one from the list view",
            dim(app),
        )));
    }
    let shown = autoscrolled(lines, inner.height as usize, app.mail_scroll);
    f.render_widget(Paragraph::new(shown), inner);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let editing = matches!(app.mode, Mode::Compose | Mode::Rename | Mode::Filter);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(match editing {
            true => Style::default().fg(app.palette.get(Role::Accent)),
            false => faint(app),
        })
        .style(Style::default().bg(app.palette.get(Role::Input)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let label = app.cursor_hint();
    let (prefix, hint) = match app.mode {
        Mode::Compose => (
            format!("to {label} › "),
            "⏎ send · ctrl+⏎ make it act".to_string(),
        ),
        Mode::Rename => (
            format!("rename {label} › "),
            "⏎ save · esc cancel".to_string(),
        ),
        Mode::Filter => ("filter › ".to_string(), "⏎ apply · esc clear".to_string()),
        Mode::List | Mode::Mail => (
            format!("to {label} › "),
            "⏎ type · m mail · n rename · o open · / filter".to_string(),
        ),
    };
    let text = app.input.clone();
    let line = Line::from(vec![
        Span::styled(
            prefix.clone(),
            Style::default().fg(app.palette.get(Role::Accent)),
        ),
        Span::styled(text.clone(), body(app)),
        Span::styled("_", faint(app)),
    ]);
    f.render_widget(Paragraph::new(line), inner);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, faint(app)))).alignment(Alignment::Right),
        inner,
    );
    if editing {
        let x = inner.x + prefix.chars().count() as u16 + text.chars().count() as u16;
        f.set_cursor_position(Position::new(
            x.min(inner.right().saturating_sub(1)),
            inner.y,
        ));
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let hint = match app.mode {
        Mode::List => {
            " ↑↓ pick · ⏎ type · m agent mail · n rename · o open · / filter · q quit ".to_string()
        }
        Mode::Compose => " ⏎ send · alt+⏎ (or ctrl+⏎) make it act · esc back ".to_string(),
        Mode::Mail => " PgUp/PgDn scroll · esc back ".to_string(),
        Mode::Rename => " ⏎ save · esc cancel ".to_string(),
        Mode::Filter => " type to filter · ⏎ apply · esc clear ".to_string(),
    };
    let line = match &app.status {
        Some(s) => Line::from(Span::styled(
            format!(" {} ", s.text),
            Style::default().fg(match s.ok {
                true => app.palette.get(Role::Success),
                false => app.palette.get(Role::Error),
            }),
        )),
        None => Line::from(Span::styled(hint, faint(app))),
    };
    f.render_widget(Paragraph::new(line), area);
}

fn short_from(name: &str) -> String {
    let tail = name.rsplit('.').next().unwrap_or(name);
    tail.chars().take(6).collect()
}

fn kfmt(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}m", n as f64 / 1_000_000.0),
        n if n >= 1000 => format!("{}k", n / 1000),
        n => n.to_string(),
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        let line_len = line.chars().count();
        if line_len > 0 && line_len + 1 + word_len > width {
            out.push(std::mem::take(&mut line));
        }
        if word_len > width {
            let mut chunk = String::new();
            for ch in word.chars() {
                chunk.push(ch);
                if chunk.chars().count() >= width {
                    out.push(std::mem::take(&mut chunk));
                }
            }
            line = chunk;
            continue;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}
