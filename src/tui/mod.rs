pub mod app;
pub mod render;
pub mod theme;

use crate::tui::app::{App, Mode};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

pub fn run(config_path: &Path) -> Result<(), String> {
    if !std::io::stdout().is_terminal() {
        return Err("`tui` needs a terminal (try `gray-comms crew --plain`)".to_string());
    }
    let config = crate::config::load_config(config_path)?;
    let mut app = App::new(config_path, config);
    let mut terminal = setup()?;
    let out = loop {
        if let Err(e) = terminal.draw(|f| render::draw(f, &mut app)) {
            break Err(format!("draw failed: {e}"));
        }
        if app.quit {
            break Ok(());
        }
        let wait = match app.pending {
            Some(_) => Duration::from_millis(100),
            None => Duration::from_millis(1000),
        };
        match event::poll(wait) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => app.on_key(key),
                Ok(_) => {}
                Err(e) => break Err(format!("input failed: {e}")),
            },
            Ok(false) => app.refresh(),
            Err(e) => break Err(format!("input failed: {e}")),
        }
    };
    restore(&mut terminal);
    out
}

fn setup() -> Result<ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>, String>
{
    crossterm::terminal::enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
    let mut out = std::io::stdout();
    crossterm::execute!(out, crossterm::terminal::EnterAlternateScreen)
        .map_err(|e| format!("alternate screen: {e}"))?;
    let backend = ratatui::backend::CrosstermBackend::new(out);
    let mut terminal = ratatui::Terminal::new(backend).map_err(|e| format!("terminal: {e}"))?;
    terminal.clear().map_err(|e| format!("clear failed: {e}"))?;
    Ok(terminal)
}

fn restore(terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>) {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = terminal.show_cursor();
}

impl App {
    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.mode {
            Mode::List => self.key_list(key),
            Mode::Compose => self.key_compose(key),
            Mode::Mail => self.key_mail(key),
            Mode::Rename => self.key_rename(key),
            Mode::Filter => self.key_filter(key),
        }
    }

    fn key_list(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    self.quit = true;
                } else {
                    self.filter.clear();
                    self.refresh();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(true),
            KeyCode::Enter => {
                if self.selected().is_some() {
                    self.input.clear();
                    self.mode = Mode::Compose;
                }
            }
            KeyCode::Char('m') => {
                self.mode = Mode::Mail;
                self.mail_scroll = 0;
                self.load_mail_now();
            }
            KeyCode::Char('n') => self.rename_start(),
            KeyCode::Char('o') => self.open_selected(),
            KeyCode::Char('/') => {
                self.input = self.filter.clone();
                self.mode = Mode::Filter;
            }
            KeyCode::PageUp => self.scroll_back = self.scroll_back.saturating_add(8),
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(8),
            _ => {}
        }
    }

    fn key_compose(&mut self, key: KeyEvent) {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.input.clear();
                self.mode = Mode::List;
            }
            KeyCode::Enter if alt || ctrl => self.send(true),
            KeyCode::Enter => self.send(false),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char('u') if ctrl => self.input.clear(),
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::PageUp => self.scroll_back = self.scroll_back.saturating_add(8),
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(8),
            _ => {}
        }
    }

    fn key_mail(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::List,
            KeyCode::Up | KeyCode::Char('k') => {
                self.mail_scroll = self.mail_scroll.saturating_add(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mail_scroll = self.mail_scroll.saturating_sub(1)
            }
            KeyCode::PageUp => self.mail_scroll = self.mail_scroll.saturating_add(10),
            KeyCode::PageDown => self.mail_scroll = self.mail_scroll.saturating_sub(10),
            _ => {}
        }
    }

    fn key_rename(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input.clear();
                self.mode = Mode::List;
            }
            KeyCode::Enter => {
                let name = self.input.clone();
                self.save_nick(&name);
                self.input.clear();
                self.mode = Mode::List;
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn key_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input.clear();
                self.filter.clear();
                self.mode = Mode::List;
                self.refresh();
            }
            KeyCode::Enter => {
                self.filter = self.input.clone();
                self.mode = Mode::List;
                self.refresh();
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.apply_filter();
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.apply_filter();
            }
            _ => {}
        }
    }
}
