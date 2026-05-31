use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use tui_input::{backend::crossterm::EventHandler, Input};
use std::io;
use tokio_stream::StreamExt;
use std::pin::Pin;
use fae_agent::{Record, SingleAgentSessionConfig, MemoryEntry};
use fae_engine::Workspace;
use unicode_width::UnicodeWidthChar;

pub struct ChatUi {
    ws: Workspace,
    agent_name: String,
    initial_chat: Option<String>,
}

impl ChatUi {
    pub fn new(ws:  Workspace, agent_name: String, initial_chat: Option<String>) -> Self {
        Self { ws, agent_name, initial_chat }
    }

    pub async fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let res = self.run_app(&mut terminal).await;

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
        terminal.show_cursor()?;

        res
    }

    async fn run_app(&mut self, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> io::Result<()> {
        let mut input = Input::default();
        let mut messages: Vec<String> = Vec::new();
        let mut scroll_offset: u16 = 0;

        let session_config = SingleAgentSessionConfig::default();
        let mut session_id = session_config.id.clone();
        let mut user_id = session_config.user_id.clone();
        let mut session = match self.ws.session_call_stream::<_, Record, Record>(
            &self.agent_name,
            session_config,
        ).await {
            Ok(s) => s,
            Err(e) => {
                messages.push(format!("Failed to create session: {:?}", e));
                return Ok(());
            }
        };

        if let Some(chat) = self.initial_chat.take() {
            input = input.with_value(chat);
        }

        let mut stream_active = false;
        let mut current_stream: Option<Pin<Box<dyn tokio_stream::Stream<Item = Record> + Send>>> = None;
        let mut auto_scroll = true;
        let mut current_title = String::new();

        loop {
            terminal.draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(1)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Min(1),
                        Constraint::Length(3),
                    ])
                    .split(f.area());

                Self::draw_status_bar(f, chunks[0], &self.agent_name, &session_id, stream_active);
                Self::draw_history(f, chunks[1], &messages, auto_scroll, &mut scroll_offset, &self.agent_name);
                Self::draw_input(f, chunks[2], &input);
            })?;

            let mut event_handled = false;
            let poll_timeout = if stream_active { 10 } else { 100 };

            if crossterm::event::poll(std::time::Duration::from_millis(poll_timeout))? {
                let event = event::read()?;
                event_handled = true;
                match event {
                    Event::Key(key) => {
                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if stream_active {
                                    stream_active = false;
                                    current_stream = None;
                                    messages.push("\n[Stream interrupted]".to_string());
                                } else {
                                    return Ok(());
                                }
                            }
                            KeyCode::Enter => {
                                let val = input.value().to_string();
                                let val = val.trim();
                                if val == "/exit" {
                                    return Ok(());
                                } else if val == "/abort" {
                                    if stream_active {
                                        stream_active = false;
                                        current_stream = None;
                                    }
                                    messages.push("\n[Session aborted. Starting a new session...]".to_string());
                                    auto_scroll = true;
                                    let session_config = SingleAgentSessionConfig::default();
                                    session_id = session_config.id.clone();
                                    user_id = session_config.user_id.clone();
                                    session = match self.ws.session_call_stream::<_, Record, Record>(
                                        &self.agent_name,
                                        session_config,
                                    ).await {
                                        Ok(s) => s,
                                        Err(e) => {
                                            messages.push(format!("Failed to create new session: {:?}", e));
                                            return Ok(());
                                        }
                                    };
                                    input.reset();
                                } else if val == "/reset" {
                                    if stream_active {
                                        stream_active = false;
                                        current_stream = None;
                                    }
                                    if let Err(e) = self.ws.session_reset(&self.agent_name, &user_id, &session_id).await {
                                        messages.push(format!("Failed to reset session: {:?}", e));
                                    } else {
                                        let text = " Session reset successfully ";
                                        let cols = crossterm::terminal::size().map(|(c, _)| c).unwrap_or(80) as usize;
                                        let inner_cols = cols.saturating_sub(4);
                                        let dashes_len = inner_cols.saturating_sub(text.len()) / 2;
                                        let dashes = "-".repeat(dashes_len);
                                        messages.push(format!("\n{}{}{}", dashes, text, dashes));
                                    }
                                    auto_scroll = true;
                                    input.reset();
                                } else if !val.is_empty() {
                                    if !stream_active {
                                        messages.push(format!("\nYou -> {}", val));
                                        auto_scroll = true;
                                        let msg = Record::from_user_input(val);
                                        match session.call_stream(msg).await {
                                            Ok(s) => {
                                                current_stream = Some(Pin::from(s));
                                                stream_active = true;
                                                current_title.clear();
                                            }
                                            Err(e) => {
                                                messages.push(format!("Failed to send chat: {:?}", e));
                                            }
                                        }
                                        input.reset();
                                    }
                                }
                            }
                            KeyCode::Up => {
                                auto_scroll = false;
                                scroll_offset = scroll_offset.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                scroll_offset = scroll_offset.saturating_add(1);
                                auto_scroll = false;
                            }
                            KeyCode::PageUp => {
                                auto_scroll = false;
                                scroll_offset = scroll_offset.saturating_sub(10);
                            }
                            KeyCode::PageDown => {
                                auto_scroll = false;
                                scroll_offset = scroll_offset.saturating_add(10);
                            }
                            KeyCode::End => {
                                auto_scroll = true;
                            }
                            _ => {
                                input.handle_event(&Event::Key(key));
                            }
                        }
                    }
                    Event::Mouse(mouse) => {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                auto_scroll = false;
                                scroll_offset = scroll_offset.saturating_sub(3);
                            }
                            MouseEventKind::ScrollDown => {
                                auto_scroll = false;
                                scroll_offset = scroll_offset.saturating_add(3);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            if stream_active {
                if let Some(ref mut st) = current_stream {
                    let select_timeout = if event_handled { 0 } else { 20 };
                    tokio::select! {
                        record_opt = st.next() => {
                            match record_opt {
                                Some(record) => {
                                    let t = record.title();
                                    if current_title != t {
                                        current_title = t;
                                        if !current_title.is_empty() {
                                            messages.push(format!("{} -> {}", self.agent_name, current_title));
                                            messages.push(String::new());
                                        }
                                    }
                                    let content = record.content();
                                    if !content.is_empty() {
                                        if let Some(last) = messages.last_mut() {
                                            last.push_str(&content);
                                        } else {
                                            messages.push(content.to_string());
                                        }
                                    }
                                }
                                None => {
                                    stream_active = false;
                                    current_stream = None;
                                }
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(select_timeout)) => {}
                    }
                } else {
                    stream_active = false;
                }
            }
        }
    }

    fn draw_status_bar(f: &mut Frame, area: Rect, agent_name: &str, session_id: &str, stream_active: bool) {
        let status_val = if stream_active {
            Span::styled("Running", Style::default().fg(Color::Green))
        } else {
            Span::styled("Idle", Style::default().fg(Color::White))
        };
        let status_line = Line::from(vec![
            Span::raw(format!(" Agent: {} | Session: {} | Status: ", agent_name, session_id)),
            status_val,
            Span::raw(" "),
        ]);
        let status_bar = Paragraph::new(status_line)
            .block(Block::default().borders(Borders::ALL).title(" Status "));
        f.render_widget(status_bar, area);
    }

    fn draw_history(f: &mut Frame, area: Rect, messages: &[String], auto_scroll: bool, scroll_offset: &mut u16, agent_name: &str) {
        let history_text = messages.join("\n");
        let agent_prefix = format!("{} ->", agent_name);
        let inner_width = area.width.saturating_sub(2).max(1) as usize;

        let mut history_spans: Vec<Line> = Vec::new();

        for line in history_text.lines() {
            let style = if line.starts_with("You ->") {
                Style::default().fg(Color::White)
            } else if line.starts_with(&agent_prefix) {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            };

            let mut current_line = String::new();
            let mut current_width = 0;

            for c in line.chars() {
                let w = c.width().unwrap_or(0);
                if current_width + w > inner_width {
                    history_spans.push(Line::from(Span::styled(current_line.clone(), style)));
                    current_line.clear();
                    current_width = 0;
                }
                current_line.push(c);
                current_width += w;
            }
            if !current_line.is_empty() || line.is_empty() {
                history_spans.push(Line::from(Span::styled(current_line, style)));
            }
        }

        let history_lines = history_spans.len() as u16;
        let inner_height = area.height.saturating_sub(2);
        let max_scroll = history_lines.saturating_sub(inner_height);

        if auto_scroll {
            *scroll_offset = max_scroll;
        } else {
            *scroll_offset = (*scroll_offset).min(max_scroll);
        }

        let history = Paragraph::new(history_spans)
            .block(Block::default().borders(Borders::ALL).title(" Chat History "))
            .scroll((*scroll_offset, 0));
        f.render_widget(history, area);
    }

    fn draw_input(f: &mut Frame, area: Rect, input: &Input) {
        let input_widget = Paragraph::new(input.value())
            .block(Block::default().borders(Borders::ALL).title(" Input (/exit to quit, /abort to restart session) "));
        f.render_widget(input_widget, area);
        
        // Cursor
        f.set_cursor_position((
            area.x + 1 + input.visual_cursor() as u16,
            area.y + 1,
        ));
    }
}