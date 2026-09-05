use std::{
    io::{self, IsTerminal, Stdout, Write},
    path::Path,
    time::Duration,
};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event as TerminalEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
    },
};
use fae_agent::{Ctx, Session, SessionEvent, SessionEventData};
use ratatui::{
    Frame, Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};
use serde_json::Value;
use tokio_stream::StreamExt;
use unicode_width::UnicodeWidthStr;

use crate::args::ColorChoice;

const SPINNER: &[&str] = &["-", "\\", "|", "/"];
const PAGE_SCROLL_LINES: u16 = 8;
const MOUSE_SCROLL_LINES: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Agent,
    Workflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunState {
    Idle,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageKind {
    User,
    Assistant,
    Reasoning,
    Tool,
    Workflow,
    System,
    Error,
}

#[derive(Debug)]
struct Message {
    kind: MessageKind,
    title: String,
    content: String,
    stream_id: Option<String>,
}

#[derive(Debug, Default)]
struct Composer {
    text: String,
    cursor: usize,
}

impl Composer {
    fn insert(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
    }

    fn backspace(&mut self) {
        if let Some(previous) = self.text[..self.cursor].char_indices().next_back() {
            self.text.drain(previous.0..self.cursor);
            self.cursor = previous.0;
        }
    }

    fn delete(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.text
                .drain(self.cursor..self.cursor.saturating_add(next.len_utf8()));
        }
    }

    fn move_left(&mut self) {
        if let Some(previous) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = previous.0;
        }
    }

    fn move_right(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map(|position| position + 1)
            .unwrap_or(0);
    }

    fn move_end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map(|offset| self.cursor + offset)
            .unwrap_or(self.text.len());
    }

    fn take(&mut self) -> Option<String> {
        let value = self.text.trim().to_string();
        self.text.clear();
        self.cursor = 0;
        (!value.is_empty()).then_some(value)
    }

    fn visual_cursor(&self, width: usize) -> (u16, u16) {
        let width = width.max(1);
        let mut row = 0usize;
        let mut column = 0usize;
        for character in self.text[..self.cursor].chars() {
            if character == '\n' {
                row += 1;
                column = 0;
                continue;
            }
            let character_width = character.to_string().width().max(1);
            if column + character_width > width {
                row += 1;
                column = 0;
            }
            column += character_width;
            if column == width {
                row += 1;
                column = 0;
            }
        }
        (column as u16, row as u16)
    }

    fn visual_lines(&self, width: usize) -> usize {
        let width = width.max(1);
        self.text
            .split('\n')
            .map(|line| line.width().max(1).div_ceil(width))
            .sum()
    }
}

#[derive(Debug)]
pub enum PromptAction {
    Submit(String),
    Exit,
}

pub struct TerminalUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    events: EventStream,
    alternate_screen: bool,
    color: bool,
    mode: Mode,
    model: String,
    subject: String,
    cwd: String,
    messages: Vec<Message>,
    composer: Composer,
    input_history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    scroll_from_bottom: u16,
    state: RunState,
    spinner: usize,
}

impl TerminalUi {
    pub fn new(
        mode: Mode,
        model: impl Into<String>,
        subject: impl Into<String>,
        color_choice: ColorChoice,
        no_alt_screen: bool,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            io::stdin().is_terminal() && io::stdout().is_terminal(),
            "interactive mode requires a terminal"
        );
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        let alternate_screen = !no_alt_screen;
        let terminal = (|| -> anyhow::Result<_> {
            execute!(stdout, EnableBracketedPaste, EnableMouseCapture)?;
            if alternate_screen {
                execute!(stdout, EnterAlternateScreen)?;
            }

            let backend = CrosstermBackend::new(stdout);
            let mut terminal = if alternate_screen {
                Terminal::new(backend)?
            } else {
                let (_, height) = size()?;
                Terminal::with_options(
                    backend,
                    TerminalOptions {
                        viewport: Viewport::Inline(height.saturating_sub(1)),
                    },
                )?
            };
            terminal.clear()?;
            Ok(terminal)
        })();
        let terminal = match terminal {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, DisableMouseCapture, DisableBracketedPaste);
                if alternate_screen {
                    let _ = execute!(stdout, LeaveAlternateScreen);
                }
                return Err(error);
            }
        };

        let color = match color_choice {
            ColorChoice::Auto => std::env::var_os("NO_COLOR").is_none(),
            ColorChoice::Always => true,
            ColorChoice::Never => false,
        };
        Ok(Self {
            terminal,
            events: EventStream::new(),
            alternate_screen,
            color,
            mode,
            model: model.into(),
            subject: subject.into(),
            cwd: display_cwd(),
            messages: Vec::new(),
            composer: Composer::default(),
            input_history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            scroll_from_bottom: 0,
            state: RunState::Idle,
            spinner: 0,
        })
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.finish_stream();
        self.messages.push(Message {
            kind: MessageKind::User,
            title: "You".to_string(),
            content: content.into(),
            stream_id: None,
        });
        self.scroll_from_bottom = 0;
    }

    pub fn push_system(&mut self, content: impl Into<String>) {
        self.finish_stream();
        self.messages.push(Message {
            kind: MessageKind::System,
            title: String::new(),
            content: content.into(),
            stream_id: None,
        });
        self.scroll_from_bottom = 0;
    }

    pub fn clear_transcript(&mut self) {
        self.messages.clear();
        self.scroll_from_bottom = 0;
    }

    pub fn workflow_result(&mut self, output: &Value) {
        self.finish_stream();
        self.messages.push(Message {
            kind: MessageKind::Workflow,
            title: "Workflow result".to_string(),
            content: pretty_value(output),
            stream_id: None,
        });
        self.state = RunState::Completed;
        self.scroll_from_bottom = 0;
    }

    pub async fn wait_for_close(&mut self) -> anyhow::Result<()> {
        loop {
            self.draw()?;
            let Some(event) = self.events.next().await else {
                return Ok(());
            };
            match event? {
                TerminalEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    match (key.code, key.modifiers) {
                        (KeyCode::Enter | KeyCode::Esc, _)
                        | (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL) => return Ok(()),
                        (KeyCode::PageUp, _) => self.scroll_up(PAGE_SCROLL_LINES),
                        (KeyCode::PageDown, _) => self.scroll_down(PAGE_SCROLL_LINES),
                        _ => {}
                    }
                }
                TerminalEvent::Mouse(mouse) => self.handle_mouse(mouse),
                TerminalEvent::Resize(_, _) => {}
                _ => {}
            }
        }
    }

    pub async fn prompt(&mut self) -> anyhow::Result<PromptAction> {
        self.state = RunState::Idle;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            self.draw()?;
            tokio::select! {
                event = self.events.next() => {
                    let Some(event) = event else {
                        return Ok(PromptAction::Exit);
                    };
                    match event? {
                        TerminalEvent::Key(key) if key.kind == KeyEventKind::Press => {
                            if let Some(action) = self.handle_prompt_key(key) {
                                return Ok(action);
                            }
                        }
                        TerminalEvent::Paste(content) => self.composer.insert(&content),
                        TerminalEvent::Mouse(mouse) => self.handle_mouse(mouse),
                        TerminalEvent::Resize(_, _) => {}
                        _ => {}
                    }
                }
                _ = tick.tick() => {}
            }
        }
    }

    pub async fn run_session<In>(
        &mut self,
        session: &impl Session<In, SessionEvent>,
        execution: Option<&Ctx>,
    ) -> anyhow::Result<bool>
    where
        In: Send + 'static,
    {
        self.state = RunState::Running;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            self.draw()?;
            tokio::select! {
                event = session.answer() => {
                    let Some(event) = event? else {
                        self.state = RunState::Idle;
                        return Ok(true);
                    };
                    let terminal = self.apply_session_event(event);
                    if terminal {
                        if self.state != RunState::Failed {
                            self.state = RunState::Idle;
                        }
                        self.draw()?;
                        return Ok(true);
                    }
                }
                input = self.events.next() => {
                    let Some(input) = input else {
                        return Ok(false);
                    };
                    match input? {
                        TerminalEvent::Key(key)
                            if key.kind == KeyEventKind::Press
                                && self.handle_running_key(key) =>
                        {
                            if let Some(execution) = execution {
                                execution.abort();
                            }
                            self.push_system("Interrupted");
                            self.state = RunState::Idle;
                            self.draw()?;
                            return Ok(false);
                        }
                        TerminalEvent::Mouse(mouse) => self.handle_mouse(mouse),
                        TerminalEvent::Resize(_, _) => {}
                        _ => {}
                    }
                }
                _ = tick.tick() => {
                    self.spinner = (self.spinner + 1) % SPINNER.len();
                }
            }
        }
    }

    pub fn apply_session_event(&mut self, event: SessionEvent) -> bool {
        let terminal = event.is_terminal();
        let stream_id = format!(
            "{}:{}:{}",
            event.node_id.as_deref().unwrap_or_default(),
            event.source,
            event.turn_id.unwrap_or_default()
        );
        match event.data {
            SessionEventData::ModelReasoning { content } => {
                self.append_stream(MessageKind::Reasoning, "Thinking", stream_id, content);
            }
            SessionEventData::ModelOutput { content } => {
                self.append_stream(MessageKind::Assistant, "Assistant", stream_id, content);
            }
            SessionEventData::ToolCall { arguments, .. } => {
                self.finish_stream();
                self.messages.push(Message {
                    kind: MessageKind::Tool,
                    title: format!("Called {}", event.source),
                    content: pretty_json_text(&arguments),
                    stream_id: None,
                });
            }
            SessionEventData::ToolOutput {
                output, completed, ..
            } => {
                self.finish_stream();
                self.messages.push(Message {
                    kind: MessageKind::Tool,
                    title: format!(
                        "{} {}",
                        if completed { "Completed" } else { "Running" },
                        event.source
                    ),
                    content: pretty_json_text(&output),
                    stream_id: None,
                });
            }
            SessionEventData::NodeCompleted { output, finished } => {
                self.finish_stream();
                self.messages.push(Message {
                    kind: MessageKind::Workflow,
                    title: if finished {
                        "Workflow complete".to_string()
                    } else {
                        format!(
                            "Completed {}",
                            event.node_id.as_deref().unwrap_or(&event.source)
                        )
                    },
                    content: if output.is_null() {
                        String::new()
                    } else {
                        pretty_value(&output)
                    },
                    stream_id: None,
                });
            }
            SessionEventData::Failed { error } => {
                self.finish_stream();
                self.messages.push(Message {
                    kind: MessageKind::Error,
                    title: "Error".to_string(),
                    content: error,
                    stream_id: None,
                });
                self.state = RunState::Failed;
            }
            SessionEventData::Custom {
                event_type,
                content,
            } => {
                self.finish_stream();
                self.messages.push(Message {
                    kind: MessageKind::System,
                    title: event_type,
                    content: compact_value(&content),
                    stream_id: None,
                });
            }
            SessionEventData::Completed { .. } => self.finish_stream(),
            SessionEventData::TurnStarted { .. } | SessionEventData::UserInput { .. } => {}
        }
        self.scroll_from_bottom = 0;
        terminal
    }

    fn append_stream(
        &mut self,
        kind: MessageKind,
        title: &str,
        stream_id: String,
        content: String,
    ) {
        append_stream_message(&mut self.messages, kind, title, stream_id, content);
    }

    fn finish_stream(&mut self) {
        if let Some(message) = self.messages.last_mut() {
            message.stream_id = None;
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Option<PromptAction> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(PromptAction::Exit);
            }
            (KeyCode::Char('d'), modifiers)
                if modifiers.contains(KeyModifiers::CONTROL) && self.composer.text.is_empty() =>
            {
                return Some(PromptAction::Exit);
            }
            (KeyCode::Char('j'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.insert("\n");
            }
            (KeyCode::Enter, modifiers)
                if modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.composer.insert("\n");
            }
            (KeyCode::Enter, _) => {
                if let Some(input) = self.composer.take() {
                    if self.input_history.last() != Some(&input) {
                        self.input_history.push(input.clone());
                    }
                    self.history_index = None;
                    self.history_draft.clear();
                    return Some(PromptAction::Submit(input));
                }
            }
            (KeyCode::Char(character), modifiers)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.composer.insert(&character.to_string());
            }
            (KeyCode::Backspace, _) => self.composer.backspace(),
            (KeyCode::Delete, _) => self.composer.delete(),
            (KeyCode::Left, _) => self.composer.move_left(),
            (KeyCode::Right, _) => self.composer.move_right(),
            (KeyCode::Home, _) => self.composer.move_home(),
            (KeyCode::End, _) => self.composer.move_end(),
            (KeyCode::Up, _) if !self.composer.text.contains('\n') => {
                self.navigate_history(true);
            }
            (KeyCode::Down, _) if !self.composer.text.contains('\n') => {
                self.navigate_history(false);
            }
            (KeyCode::PageUp, _) => self.scroll_up(PAGE_SCROLL_LINES),
            (KeyCode::PageDown, _) => self.scroll_down(PAGE_SCROLL_LINES),
            _ => {}
        }
        None
    }

    fn navigate_history(&mut self, older: bool) {
        if self.input_history.is_empty() {
            return;
        }
        let next = if older {
            match self.history_index {
                Some(0) => 0,
                Some(index) => index - 1,
                None => {
                    self.history_draft = self.composer.text.clone();
                    self.input_history.len() - 1
                }
            }
        } else {
            match self.history_index {
                Some(index) if index + 1 < self.input_history.len() => index + 1,
                Some(_) => {
                    self.history_index = None;
                    self.composer.text = std::mem::take(&mut self.history_draft);
                    self.composer.cursor = self.composer.text.len();
                    return;
                }
                None => return,
            }
        };
        self.history_index = Some(next);
        self.composer.text = self.input_history[next].clone();
        self.composer.cursor = self.composer.text.len();
    }

    fn handle_running_key(&mut self, key: KeyEvent) -> bool {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => true,
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => true,
            (KeyCode::PageUp, _) => {
                self.scroll_up(PAGE_SCROLL_LINES);
                false
            }
            (KeyCode::PageDown, _) => {
                self.scroll_down(PAGE_SCROLL_LINES);
                false
            }
            _ => false,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_up(MOUSE_SCROLL_LINES),
            MouseEventKind::ScrollDown => self.scroll_down(MOUSE_SCROLL_LINES),
            _ => {}
        }
    }

    fn scroll_up(&mut self, lines: u16) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(lines);
    }

    fn scroll_down(&mut self, lines: u16) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(lines);
    }

    fn draw(&mut self) -> anyhow::Result<()> {
        let color = self.color;
        let mode = self.mode;
        let model = self.model.clone();
        let subject = self.subject.clone();
        let cwd = self.cwd.clone();
        let state = self.state;
        let spinner = self.spinner;
        let messages = &self.messages;
        let composer = &self.composer;
        let scroll_from_bottom = self.scroll_from_bottom;

        self.terminal.draw(|frame| {
            draw_frame(
                frame,
                ViewModel {
                    color,
                    mode,
                    model: &model,
                    subject: &subject,
                    cwd: &cwd,
                    state,
                    spinner,
                    messages,
                    composer,
                    scroll_from_bottom,
                },
            );
        })?;
        Ok(())
    }
}

impl Drop for TerminalUi {
    fn drop(&mut self) {
        let transcript = self
            .alternate_screen
            .then(|| plain_transcript(&self.messages));
        let _ = self.terminal.show_cursor();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        if self.alternate_screen {
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        }
        let _ = disable_raw_mode();

        if let Some(transcript) = transcript
            && !transcript.is_empty()
        {
            let mut stdout = io::stdout();
            let _ = writeln!(stdout, "{transcript}");
            let _ = stdout.flush();
        }
    }
}

struct ViewModel<'a> {
    color: bool,
    mode: Mode,
    model: &'a str,
    subject: &'a str,
    cwd: &'a str,
    state: RunState,
    spinner: usize,
    messages: &'a [Message],
    composer: &'a Composer,
    scroll_from_bottom: u16,
}

fn draw_frame(frame: &mut Frame<'_>, view: ViewModel<'_>) {
    let composer_width = frame.area().width.saturating_sub(4).max(1) as usize;
    let composer_height = (view.composer.visual_lines(composer_width) as u16 + 2).clamp(3, 8);
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .split(frame.area());

    draw_header(frame, areas[0], &view);
    draw_transcript(frame, areas[1], &view);
    draw_composer(frame, areas[2], &view);
    draw_footer(frame, areas[3], &view);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, view: &ViewModel<'_>) {
    let accent = color(view.color, Color::Cyan);
    let title = Line::from(vec![
        Span::styled(" FAE ", Style::default().fg(Color::Black).bg(accent).bold()),
        Span::styled(
            match view.mode {
                Mode::Agent => "  Agent",
                Mode::Workflow => "  Workflow",
            },
            Style::default().fg(accent).bold(),
        ),
        Span::raw(format!("  {}", view.subject)),
    ]);
    let details = Line::from(vec![
        Span::styled(
            " model ",
            Style::default().fg(color(view.color, Color::DarkGray)),
        ),
        Span::raw(view.model),
        Span::styled(
            "  cwd ",
            Style::default().fg(color(view.color, Color::DarkGray)),
        ),
        Span::raw(view.cwd),
    ]);
    frame.render_widget(Paragraph::new(vec![title, details]), area);
}

fn draw_transcript(frame: &mut Frame<'_>, area: Rect, view: &ViewModel<'_>) {
    let text = transcript_text(view.messages, view.color);
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .block(Block::default().padding(Padding::horizontal(1)));
    let width = area.width.saturating_sub(2);
    let total_lines = transcript_line_count(view.messages, width as usize);
    let max_scroll = total_lines.saturating_sub(area.height);
    let scroll = max_scroll.saturating_sub(view.scroll_from_bottom.min(max_scroll));
    frame.render_widget(paragraph.scroll((scroll, 0)), area);
}

fn draw_composer(frame: &mut Frame<'_>, area: Rect, view: &ViewModel<'_>) {
    let accent = color(view.color, Color::Cyan);
    let (title, border, content) = match view.state {
        RunState::Idle => (
            " Message ",
            accent,
            if view.composer.text.is_empty() {
                "Ask FAE to work in this repository".to_string()
            } else {
                view.composer.text.clone()
            },
        ),
        RunState::Running => (
            " Working ",
            color(view.color, Color::Yellow),
            format!("{} Running... Esc to interrupt", SPINNER[view.spinner]),
        ),
        RunState::Completed => (
            " Done ",
            color(view.color, Color::Green),
            "Press Enter to close".to_string(),
        ),
        RunState::Failed => (
            " Message ",
            color(view.color, Color::Red),
            view.composer.text.clone(),
        ),
    };
    let content_style = if view.state == RunState::Idle && view.composer.text.is_empty() {
        Style::default().fg(color(view.color, Color::DarkGray))
    } else {
        Style::default()
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .padding(Padding::horizontal(1));
    frame.render_widget(
        Paragraph::new(content)
            .style(content_style)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );

    if matches!(view.state, RunState::Idle | RunState::Failed) {
        let inner_width = area.width.saturating_sub(4).max(1) as usize;
        let (cursor_x, cursor_y) = view.composer.visual_cursor(inner_width);
        frame.set_cursor_position(Position::new(
            area.x + 2 + cursor_x,
            (area.y + 1 + cursor_y).min(area.bottom().saturating_sub(2)),
        ));
    }
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, view: &ViewModel<'_>) {
    let hint = match view.state {
        RunState::Running => " Esc interrupt   Wheel/PgUp/PgDn scroll ",
        RunState::Completed => " Enter close   Wheel/PgUp/PgDn scroll ",
        _ => " Enter send   Ctrl+J newline   Wheel/PgUp/PgDn scroll   Ctrl+C quit ",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(color(view.color, Color::DarkGray)),
        ))),
        area,
    );
}

fn transcript_text(messages: &[Message], use_color: bool) -> Text<'static> {
    let mut lines = Vec::new();
    if messages.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Start by entering a task below.",
            Style::default().fg(color(use_color, Color::DarkGray)),
        )));
        return Text::from(lines);
    }

    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        let (marker, marker_color, modifier) = match message.kind {
            MessageKind::User => (">", Color::Cyan, Modifier::BOLD),
            MessageKind::Assistant => ("*", Color::Green, Modifier::BOLD),
            MessageKind::Reasoning => ("-", Color::DarkGray, Modifier::ITALIC),
            MessageKind::Tool => ("$", Color::Yellow, Modifier::BOLD),
            MessageKind::Workflow => ("+", Color::Green, Modifier::BOLD),
            MessageKind::System => ("-", Color::DarkGray, Modifier::DIM),
            MessageKind::Error => ("!", Color::Red, Modifier::BOLD),
        };
        let heading = if message.title.is_empty() {
            marker.to_string()
        } else {
            format!("{marker} {}", message.title)
        };
        lines.push(Line::from(Span::styled(
            heading,
            Style::default()
                .fg(color(use_color, marker_color))
                .add_modifier(modifier),
        )));
        if !message.content.is_empty() {
            for content_line in message.content.lines() {
                lines.push(Line::from(format!("  {content_line}")));
            }
        }
    }
    Text::from(lines)
}

fn plain_transcript(messages: &[Message]) -> String {
    messages
        .iter()
        .map(|message| {
            let marker = match message.kind {
                MessageKind::User => ">",
                MessageKind::Assistant => "*",
                MessageKind::Reasoning => "-",
                MessageKind::Tool => "$",
                MessageKind::Workflow => "+",
                MessageKind::System => "-",
                MessageKind::Error => "!",
            };
            let heading = if message.title.is_empty() {
                marker.to_string()
            } else {
                format!("{marker} {}", message.title)
            };
            if message.content.is_empty() {
                heading
            } else {
                format!(
                    "{heading}\n{}",
                    message
                        .content
                        .lines()
                        .map(|line| format!("  {line}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn transcript_line_count(messages: &[Message], width: usize) -> u16 {
    if messages.is_empty() {
        return 2;
    }
    let width = width.max(1);
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let spacing = usize::from(index > 0);
            let heading_width = message.title.width().saturating_add(2);
            let heading_lines = heading_width.max(1).div_ceil(width);
            let content_lines: usize = message
                .content
                .lines()
                .map(|line| line.width().saturating_add(2).max(1).div_ceil(width))
                .sum();
            spacing + heading_lines + content_lines
        })
        .sum::<usize>()
        .min(u16::MAX as usize) as u16
}

fn color(enabled: bool, value: Color) -> Color {
    if enabled { value } else { Color::Reset }
}

fn append_stream_message(
    messages: &mut Vec<Message>,
    kind: MessageKind,
    title: &str,
    stream_id: String,
    content: String,
) {
    let current_matches = messages.last().is_some_and(|message| {
        message.kind == kind && message.stream_id.as_ref() == Some(&stream_id)
    });
    if current_matches {
        messages
            .last_mut()
            .expect("last message exists")
            .content
            .push_str(&content);
    } else {
        if let Some(message) = messages.last_mut() {
            message.stream_id = None;
        }
        messages.push(Message {
            kind,
            title: title.to_string(),
            content,
            stream_id: Some(stream_id),
        });
    }
}

fn pretty_json_text(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .map(|value| pretty_value(&value))
        .unwrap_or_else(|_| text.to_string())
}

fn pretty_value(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn compact_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn display_cwd() -> String {
    std::env::current_dir()
        .unwrap_or_else(|_| Path::new(".").to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use fae_agent::SessionEventData;
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn composer_edits_unicode_at_character_boundaries() {
        let mut composer = Composer::default();
        composer.insert("a你");
        composer.move_left();
        composer.backspace();
        composer.insert("b");

        assert_eq!(composer.text, "b你");
        assert_eq!(composer.cursor, 1);
    }

    #[test]
    fn streaming_chunks_merge_into_one_message() {
        let mut messages = Vec::new();
        append_stream_message(
            &mut messages,
            MessageKind::Assistant,
            "Assistant",
            "stream".to_string(),
            "hel".to_string(),
        );
        append_stream_message(
            &mut messages,
            MessageKind::Assistant,
            "Assistant",
            "stream".to_string(),
            "lo".to_string(),
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "hello");
    }

    #[test]
    fn compact_layout_renders_without_overlap() {
        let backend = TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let messages = vec![Message {
            kind: MessageKind::Assistant,
            title: "Assistant".to_string(),
            content: "A response that remains visible above the composer.".to_string(),
            stream_id: None,
        }];
        let composer = Composer::default();

        terminal
            .draw(|frame| {
                draw_frame(
                    frame,
                    ViewModel {
                        color: false,
                        mode: Mode::Agent,
                        model: "test-model",
                        subject: "test-session",
                        cwd: "/workspace",
                        state: RunState::Idle,
                        spinner: 0,
                        messages: &messages,
                        composer: &composer,
                        scroll_from_bottom: 0,
                    },
                )
            })
            .unwrap();

        let content = terminal.backend().to_string();
        assert!(content.contains("A response that remains visible"));
        assert!(content.contains("Message"));
        assert!(content.contains("Enter send"));
    }

    #[test]
    fn plain_transcript_preserves_visible_conversation() {
        let messages = vec![
            Message {
                kind: MessageKind::User,
                title: "You".to_string(),
                content: "Question".to_string(),
                stream_id: None,
            },
            Message {
                kind: MessageKind::Assistant,
                title: "Assistant".to_string(),
                content: "First line\nSecond line".to_string(),
                stream_id: None,
            },
        ];

        assert_eq!(
            plain_transcript(&messages),
            "> You\n  Question\n\n* Assistant\n  First line\n  Second line"
        );
    }

    #[test]
    fn terminal_event_marks_failed_state() {
        let event = SessionEvent::single_agent(
            1,
            "fae",
            SessionEventData::Failed {
                error: "boom".to_string(),
            },
        );
        assert!(event.is_terminal());
    }
}
