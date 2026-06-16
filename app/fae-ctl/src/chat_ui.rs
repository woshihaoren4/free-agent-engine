use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use fae_agent::{GLOBAL_KEY_PROJECT_DIR, MemoryEntry, ModelCallConfig, Record, SingleSessionMD};
use fae_engine::Workspace;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::io;
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::StreamExt;
use tui_input::{
    Input, InputRequest,
    backend::crossterm::{EventHandler, to_input_request},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

pub struct ChatUi {
    ws: Workspace,
    agent_name: String,
    model_name: ModelCallConfig,
}

impl ChatUi {
    pub fn new(ws: Workspace, agent_name: String) -> Self {
        let model_name = ModelCallConfig::default();
        Self {
            ws,
            agent_name,
            model_name,
        }
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        let agent = self.ws.get_agent(&self.agent_name).await?.on_info().await;
        self.model_name = agent.model();

        let guard = TerminalGuard::enter()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        let res = self.run_app(&mut terminal).await;

        let _ = terminal.show_cursor();
        drop(terminal);
        drop(guard);

        res
    }

    async fn run_app(&mut self, terminal: &mut Tui) -> anyhow::Result<()> {
        let mut state = ChatState::default();
        let session_config = Self::new_session_config();
        let mut session_id = session_config.id.clone();
        let mut user_id = session_config.user_id.clone();
        let mut session = self
            .ws
            .session_call_stream::<_, Record, Record>(&self.agent_name, session_config)
            .await?;

        let mut stream_active = false;
        let mut current_stream: Option<Pin<Box<dyn tokio_stream::Stream<Item = Record> + Send>>> =
            None;

        loop {
            terminal.draw(|frame| self.render(frame, &mut state))?;

            if event::poll(Duration::from_millis(if stream_active { 30 } else { 120 }))? {
                if let Event::Key(key) = event::read()? {
                    if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                        continue;
                    }

                    match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if !stream_active {
                                return Ok(());
                            }
                            stream_active = false;
                            current_stream = None;
                            state.current_title.clear();
                            state.push_system("Session aborted. Starting a new session.");
                            let session_config = Self::new_session_config();
                            session_id = session_config.id.clone();
                            user_id = session_config.user_id.clone();
                            session = self
                                .ws
                                .session_call_stream::<_, Record, Record>(
                                    &self.agent_name,
                                    session_config,
                                )
                                .await?;
                            state.input.reset();
                            state.status = "Ready".to_string();
                        }
                        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.input.handle(InputRequest::InsertChar('\n'));
                        }
                        KeyCode::PageUp => state.scroll_up(),
                        KeyCode::PageDown => state.scroll_down(),
                        KeyCode::Enter => {
                            let raw = state.input.value().to_string();
                            let val = raw.trim();
                            if val.starts_with("/exit") {
                                return Ok(());
                            } else if val.starts_with("/reset") {
                                stream_active = false;
                                current_stream = None;
                                state.input.reset();
                                state.current_title.clear();
                                if let Err(e) = self
                                    .ws
                                    .session_reset(&self.agent_name, &user_id, &session_id)
                                    .await
                                {
                                    state.push_error(format!("Failed to reset session: {e:?}"));
                                } else {
                                    state.messages.clear();
                                    state.push_system("Session reset successfully.");
                                    state.status = "Ready".to_string();
                                }
                            } else if !val.is_empty() {
                                if stream_active {
                                    state.status =
                                        "Assistant is still responding. Ctrl+C aborts it."
                                            .to_string();
                                } else {
                                    let msg = raw.trim().to_string();
                                    state.input.reset();
                                    state.push_user(msg.clone());
                                    match session.call_stream(Record::from_user_input(msg)).await {
                                        Ok(s) => {
                                            current_stream = Some(Pin::from(s));
                                            stream_active = true;
                                            state.current_title = "Waiting".to_string();
                                            state.status = "Waiting".to_string();
                                        }
                                        Err(e) => {
                                            state.push_error(format!("Failed to send chat: {e:?}"));
                                            state.status = "Ready".to_string();
                                        }
                                    }
                                }
                            }
                        }
                        _ => {
                            if to_input_request(&Event::Key(key)).is_some() {
                                state.input.handle_event(&Event::Key(key));
                            }
                        }
                    }
                }
            }

            if stream_active {
                let mut done = false;
                if let Some(stream) = current_stream.as_mut() {
                    match tokio::time::timeout(Duration::from_millis(20), stream.next()).await {
                        Ok(Some(record)) => {
                            let title = record.title();
                            state.current_title = title.clone();
                            state.status = title;
                            state.push_record(&record, &self.agent_name);
                        }
                        Ok(None) => done = true,
                        Err(_) => {}
                    }
                } else {
                    done = true;
                }

                if done {
                    stream_active = false;
                    current_stream = None;
                    state.current_title.clear();
                    state.status = "Ready".to_string();
                }
                state.spinner_tick = state.spinner_tick.wrapping_add(1);
            }
        }
    }

    fn render(&self, frame: &mut Frame<'_>, state: &mut ChatState) {
        let area = frame.area();
        let input_height = Self::input_height(state.input.value(), area.width);
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(area);

        self.render_header(frame, chunks[0]);
        self.render_history(frame, chunks[1], state);
        self.render_status(frame, chunks[2], state);
        self.render_input(frame, chunks[3], state);
        self.render_help(frame, chunks[4]);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let dir = compact_current_dir();
        let header = Text::from(vec![
            Line::from(vec![
                Span::styled(
                    ">_ ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Free Agent Engine",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  v{}", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(vec![
                Span::styled("agent ", Style::default().fg(Color::DarkGray)),
                Span::styled(&self.agent_name, Style::default().fg(Color::Green)),
                Span::styled("  model ", Style::default().fg(Color::DarkGray)),
                Span::styled(&self.model_name.model, Style::default().fg(Color::Yellow)),
                Span::styled("  dir ", Style::default().fg(Color::DarkGray)),
                Span::styled(dir, Style::default().fg(Color::White)),
            ]),
        ]);
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));
        frame.render_widget(Paragraph::new(header).block(block), area);
    }

    fn render_history(&self, frame: &mut Frame<'_>, area: Rect, state: &mut ChatState) {
        let mut lines = Vec::new();
        if state.messages.is_empty() {
            lines.push(Line::styled(
                "Ask a question to start.",
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            for message in &state.messages {
                lines.extend(message.render_lines(&self.agent_name));
            }
        }

        let height = wrapped_height(&lines, area.width as usize);
        let max_scroll = height.saturating_sub(area.height as usize) as u16;
        if state.follow_tail {
            state.scroll = max_scroll;
        } else {
            state.scroll = state.scroll.min(max_scroll);
        }

        let history = Paragraph::new(Text::from(lines))
            .scroll((state.scroll, 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(history, area);
    }

    fn render_status(&self, frame: &mut Frame<'_>, area: Rect, state: &ChatState) {
        let status = if state.current_title.is_empty() {
            state.status.clone()
        } else {
            let spinners = ["|", "/", "-", "\\"];
            format!(
                "{} {}",
                spinners[(state.spinner_tick / 2) % spinners.len()],
                state.current_title
            )
        };
        let line = Line::from(vec![
            Span::styled(" ", Style::default().bg(Color::DarkGray)),
            Span::styled(
                status,
                Style::default().fg(Color::Black).bg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_input(&self, frame: &mut Frame<'_>, area: Rect, state: &ChatState) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " Message ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let text = if state.input.value().is_empty() {
            Text::from(Line::styled(
                "Type a message...",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            Text::from(state.input.value().to_string())
        };
        frame.render_widget(
            Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
            area,
        );
        let (x, y) = input_cursor_position(state.input.value(), state.input.cursor(), inner);
        frame.set_cursor_position((x, y));
    }

    fn render_help(&self, frame: &mut Frame<'_>, area: Rect) {
        let help = Line::from(vec![
            Span::styled(" Enter ", Style::default().fg(Color::White)),
            Span::styled("send", Style::default().fg(Color::DarkGray)),
            Span::styled("  Ctrl+J ", Style::default().fg(Color::White)),
            Span::styled("newline", Style::default().fg(Color::DarkGray)),
            Span::styled("  /reset ", Style::default().fg(Color::White)),
            Span::styled("reset", Style::default().fg(Color::DarkGray)),
            Span::styled("  Ctrl+C ", Style::default().fg(Color::White)),
            Span::styled("abort/quit", Style::default().fg(Color::DarkGray)),
            Span::styled("  PgUp/PgDn ", Style::default().fg(Color::White)),
            Span::styled("scroll", Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(help), area);
    }

    fn input_height(value: &str, terminal_width: u16) -> u16 {
        let inner_width = terminal_width.saturating_sub(4).max(1) as usize;
        let rows = value
            .split('\n')
            .map(|line| (UnicodeWidthStr::width(line).max(1) + inner_width - 1) / inner_width)
            .sum::<usize>()
            .clamp(1, 6);
        rows as u16 + 2
    }

    fn new_session_config() -> SingleSessionMD {
        SingleSessionMD::default().set(GLOBAL_KEY_PROJECT_DIR, ".")
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

struct ChatState {
    input: Input,
    messages: Vec<ChatMessage>,
    status: String,
    current_title: String,
    spinner_tick: usize,
    scroll: u16,
    follow_tail: bool,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            input: Input::default(),
            messages: Vec::new(),
            status: "Ready".to_string(),
            current_title: String::new(),
            spinner_tick: 0,
            scroll: 0,
            follow_tail: true,
        }
    }
}

impl ChatState {
    fn push_user(&mut self, content: String) {
        self.messages.push(ChatMessage {
            role: MessageRole::User,
            title: "You".to_string(),
            agent_id: String::new(),
            content,
        });
        self.follow_tail = true;
    }

    fn push_system(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: MessageRole::System,
            title: "System".to_string(),
            agent_id: String::new(),
            content: content.into(),
        });
        self.follow_tail = true;
    }

    fn push_error(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: MessageRole::Error,
            title: "Error".to_string(),
            agent_id: String::new(),
            content: content.into(),
        });
        self.follow_tail = true;
    }

    fn push_record(&mut self, record: &Record, fallback_agent: &str) {
        let content = record.content();
        if content.is_empty() {
            return;
        }

        let title = record.title();
        let role = MessageRole::from_title(&title);
        let agent_id = if record.agent_id.is_empty() {
            fallback_agent.to_string()
        } else {
            record.agent_id.clone()
        };

        if let Some(last) = self.messages.last_mut() {
            if last.role == role
                && last.agent_id == agent_id
                && role.can_append(&last.title, &title)
            {
                last.title = title;
                last.content.push_str(content);
                self.follow_tail = true;
                return;
            }
        }

        self.messages.push(ChatMessage {
            role,
            title,
            agent_id,
            content: content.to_string(),
        });
        self.follow_tail = true;
    }

    fn scroll_up(&mut self) {
        self.follow_tail = false;
        self.scroll = self.scroll.saturating_sub(8);
    }

    fn scroll_down(&mut self) {
        self.follow_tail = false;
        self.scroll = self.scroll.saturating_add(8);
    }
}

struct ChatMessage {
    role: MessageRole,
    title: String,
    agent_id: String,
    content: String,
}

impl ChatMessage {
    fn render_lines(&self, fallback_agent: &str) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let (label, color) = match self.role {
            MessageRole::User => ("You".to_string(), Color::Cyan),
            MessageRole::Assistant => {
                let agent = if self.agent_id.is_empty() {
                    fallback_agent
                } else {
                    &self.agent_id
                };
                (agent.to_string(), Color::Green)
            }
            MessageRole::Thought => ("thinking".to_string(), Color::DarkGray),
            MessageRole::Tool => (self.title.clone(), Color::Yellow),
            MessageRole::System => ("system".to_string(), Color::Blue),
            MessageRole::Error => ("error".to_string(), Color::Red),
        };

        lines.push(Line::from(vec![
            Span::styled(
                "› ",
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ]));

        let body_style = self.role.body_style();
        for line in self.content.split('\n') {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(line.to_string(), body_style),
            ]));
        }
        lines.push(Line::raw(""));
        lines
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageRole {
    User,
    Assistant,
    Thought,
    Tool,
    System,
    Error,
}

impl MessageRole {
    fn from_title(title: &str) -> Self {
        if title == "Thinking" {
            Self::Thought
        } else if title.starts_with("CallTool") || title.starts_with("ToolOut") {
            Self::Tool
        } else {
            Self::Assistant
        }
    }

    fn can_append(self, previous_title: &str, next_title: &str) -> bool {
        match self {
            Self::Tool => {
                previous_title.starts_with("CallTool") == next_title.starts_with("CallTool")
                    && previous_title.starts_with("ToolOut") == next_title.starts_with("ToolOut")
            }
            Self::Assistant | Self::Thought => previous_title == next_title,
            Self::User | Self::System | Self::Error => false,
        }
    }

    fn body_style(self) -> Style {
        match self {
            Self::Thought => Style::default().fg(Color::DarkGray),
            Self::Tool => Style::default().fg(Color::Yellow),
            Self::Error => Style::default().fg(Color::Red),
            _ => Style::default().fg(Color::White),
        }
    }
}

fn wrapped_height(lines: &[Line<'_>], width: usize) -> usize {
    let width = width.max(1);
    lines
        .iter()
        .map(|line| {
            let line_width = line.width();
            if line_width == 0 {
                1
            } else {
                (line_width + width - 1) / width
            }
        })
        .sum()
}

fn input_cursor_position(value: &str, cursor: usize, area: Rect) -> (u16, u16) {
    let width = area.width.max(1) as usize;
    let mut x = 0usize;
    let mut y = 0usize;

    for ch in value.chars().take(cursor) {
        if ch == '\n' {
            x = 0;
            y += 1;
            continue;
        }

        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if x + ch_width > width {
            x = 0;
            y += 1;
        }
        x += ch_width;
    }

    let max_y = area.height.saturating_sub(1) as usize;
    (
        area.x.saturating_add(x.min(width.saturating_sub(1)) as u16),
        area.y.saturating_add(y.min(max_y) as u16),
    )
}

fn compact_current_dir() -> String {
    std::env::current_dir()
        .ok()
        .map(|path| {
            let path = path.display().to_string();
            match std::env::var("HOME") {
                Ok(home) if path.starts_with(&home) => format!("~{}", &path[home.len()..]),
                _ => path,
            }
        })
        .unwrap_or_else(|| ".".to_string())
}
