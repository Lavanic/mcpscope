//! Minimal session browser: sessions, then a session's timeline, then one
//! message in full. Deliberately small; tail and stats carry the daily
//! workload, this is for digging after the fact.

use crate::db;
use crate::display::{LOCAL_HMS, LOCAL_STAMP, local_time_sql, sanitize_block, sanitize_line};
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use rusqlite::{Connection, params};

struct Session {
    id: i64,
    start: String,
    server: String,
    client: String,
    messages: i64,
}

struct Msg {
    time: String,
    direction: String,
    kind: String,
    method: Option<String>,
    rpc_id: Option<String>,
    body: String,
    latency_ms: Option<i64>,
    error_msg: Option<String>,
    truncated: bool,
}

enum Screen {
    Sessions,
    Timeline,
    Detail,
}

struct App {
    conn: Connection,
    screen: Screen,
    sessions: Vec<Session>,
    selected_session: usize,
    msgs: Vec<Msg>,
    selected_msg: usize,
    errors_only: bool,
    scroll: u16,
}

pub fn run() -> Result<()> {
    let conn = db::open(&db::default_path()?)?;
    let sessions = load_sessions(&conn)?;
    let app = App {
        conn,
        screen: Screen::Sessions,
        sessions,
        selected_session: 0,
        msgs: Vec::new(),
        selected_msg: 0,
        errors_only: false,
        scroll: 0,
    };
    // init/restore switch to the terminal's alternate screen and back, and a
    // panic hook makes sure a crash cannot leave the terminal unusable
    let mut terminal = ratatui::init();
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

impl App {
    fn event_loop(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame| self.render(frame))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if matches!(key.code, KeyCode::Char('q')) {
                return Ok(());
            }
            match self.screen {
                Screen::Sessions => match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.selected_session = step(self.selected_session, 1, self.sessions.len());
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.selected_session =
                            step(self.selected_session, -1, self.sessions.len());
                    }
                    KeyCode::Enter if !self.sessions.is_empty() => {
                        self.load_msgs()?;
                        self.selected_msg = 0;
                        self.screen = Screen::Timeline;
                    }
                    _ => {}
                },
                Screen::Timeline => match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.selected_msg = step(self.selected_msg, 1, self.msgs.len());
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.selected_msg = step(self.selected_msg, -1, self.msgs.len());
                    }
                    KeyCode::Char('e') => {
                        self.errors_only = !self.errors_only;
                        self.load_msgs()?;
                        self.selected_msg = 0;
                    }
                    KeyCode::Enter if !self.msgs.is_empty() => {
                        self.scroll = 0;
                        self.screen = Screen::Detail;
                    }
                    KeyCode::Esc | KeyCode::Backspace => self.screen = Screen::Sessions,
                    _ => {}
                },
                Screen::Detail => match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.scroll = self.scroll.saturating_add(1)
                    }
                    KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                    KeyCode::Esc | KeyCode::Backspace => self.screen = Screen::Timeline,
                    _ => {}
                },
            }
        }
    }

    fn load_msgs(&mut self) -> Result<()> {
        let session = self.sessions[self.selected_session].id;
        self.msgs = load_msgs(&self.conn, session, self.errors_only)?;
        Ok(())
    }

    fn render(&self, frame: &mut Frame) {
        let [main, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        match self.screen {
            Screen::Sessions => {
                let items: Vec<ListItem> = self
                    .sessions
                    .iter()
                    .map(|s| {
                        ListItem::new(format!(
                            "{:>4}  {}  {:<14} {:<24} {:>6} msgs",
                            s.id, s.start, s.server, s.client, s.messages
                        ))
                    })
                    .collect();
                let title = if items.is_empty() {
                    " no recorded sessions yet "
                } else {
                    " sessions "
                };
                let list = List::new(items)
                    .block(Block::bordered().title(title))
                    .highlight_style(Style::new().bg(Color::DarkGray));
                let mut state = ListState::default().with_selected(Some(self.selected_session));
                frame.render_stateful_widget(list, main, &mut state);
                frame.render_widget(Line::raw(" j/k move   enter open   q quit"), footer);
            }
            Screen::Timeline => {
                let items: Vec<ListItem> = self.msgs.iter().map(timeline_item).collect();
                let session = &self.sessions[self.selected_session];
                let title = format!(
                    " {} / session {}{} ",
                    session.server,
                    session.id,
                    if self.errors_only {
                        " / errors only"
                    } else {
                        ""
                    }
                );
                let list = List::new(items)
                    .block(Block::bordered().title(title))
                    .highlight_style(Style::new().bg(Color::DarkGray));
                let mut state = ListState::default().with_selected(Some(self.selected_msg));
                frame.render_stateful_widget(list, main, &mut state);
                frame.render_widget(
                    Line::raw(" j/k move   enter detail   e errors   esc back   q quit"),
                    footer,
                );
            }
            Screen::Detail => {
                let msg = &self.msgs[self.selected_msg];
                let title = format!(
                    " {} {} {} ",
                    msg.direction,
                    msg.kind,
                    msg.method.as_deref().unwrap_or("")
                );
                let mut text = pretty_body(&msg.body);
                if msg.truncated {
                    text.push_str("\n\n[stored body truncated by --max-body]");
                }
                let para = Paragraph::new(text)
                    .block(Block::bordered().title(title))
                    .scroll((self.scroll, 0));
                frame.render_widget(para, main);
                frame.render_widget(Line::raw(" j/k scroll   esc back   q quit"), footer);
            }
        }
    }
}

/// One timeline line, colored like tail: noise yellow, errors red,
/// requests green.
fn timeline_item(msg: &Msg) -> ListItem<'_> {
    let arrow = if msg.direction == "c2s" { ">>" } else { "<<" };
    let id = msg
        .rpc_id
        .as_deref()
        .map(|id| format!(" id={id}"))
        .unwrap_or_default();
    let latency = msg
        .latency_ms
        .map(|ms| format!(" ({ms}ms)"))
        .unwrap_or_default();
    // all of this came off the wire; none of it is rendered raw
    let rest = match msg.kind.as_str() {
        "request" | "notification" => {
            format!("{}{id}", sanitize_line(msg.method.as_deref().unwrap_or("")))
        }
        "response" => format!(
            "{}{id}{latency} {}",
            sanitize_line(msg.method.as_deref().unwrap_or("?")),
            sanitize_line(msg.error_msg.as_deref().unwrap_or(""))
        ),
        _ => sanitize_line(&msg.body.chars().take(120).collect::<String>()),
    };
    let line = format!("{} {arrow} {:<12} {rest}", msg.time, msg.kind);
    let style = match msg.kind.as_str() {
        "noise" => Style::new().fg(Color::Yellow),
        "request" => Style::new().fg(Color::Green),
        "response" if msg.error_msg.is_some() => Style::new().fg(Color::Red),
        _ => Style::new(),
    };
    ListItem::new(line).style(style)
}

fn pretty_body(body: &str) -> String {
    let text = match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_owned()),
        Err(_) => body.to_owned(),
    };
    sanitize_block(&text)
}

/// Move a selection up or down without running off either end.
fn step(current: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    current.saturating_add_signed(delta).min(len - 1)
}

fn load_sessions(conn: &Connection) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT s.id, {}, s.server_name,
                COALESCE(s.client, '?'), COUNT(m.id)
         FROM sessions s LEFT JOIN messages m ON m.session_id = s.id
         GROUP BY s.id ORDER BY s.id DESC",
        local_time_sql(LOCAL_STAMP, "s.started_at")
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok(Session {
            id: row.get(0)?,
            start: row.get(1)?,
            server: row.get(2)?,
            client: row.get(3)?,
            messages: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn load_msgs(conn: &Connection, session: i64, errors_only: bool) -> Result<Vec<Msg>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {}, direction, kind, method, rpc_id, body, latency_ms, error_msg, truncated
         FROM messages
         WHERE session_id = ?1 AND (?2 = 0 OR kind = 'noise' OR error_code IS NOT NULL)
         ORDER BY id",
        local_time_sql(LOCAL_HMS, "ts_ms")
    ))?;
    let rows = stmt.query_map(params![session, errors_only], |row| {
        Ok(Msg {
            time: row.get(0)?,
            direction: row.get(1)?,
            kind: row.get(2)?,
            method: row.get(3)?,
            rpc_id: row.get(4)?,
            body: row.get(5)?,
            latency_ms: row.get(6)?,
            error_msg: row.get(7)?,
            truncated: row.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_stays_in_bounds() {
        assert_eq!(step(0, -1, 5), 0);
        assert_eq!(step(4, 1, 5), 4);
        assert_eq!(step(2, 1, 5), 3);
        assert_eq!(step(0, 1, 0), 0);
    }

    #[test]
    fn pretty_body_survives_garbage() {
        assert_eq!(pretty_body("not json"), "not json");
        assert!(pretty_body("{\"a\":1}").contains("\"a\": 1"));
    }
}
