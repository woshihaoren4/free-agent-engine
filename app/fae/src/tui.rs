use std::{
    collections::HashMap,
    io::{self, IsTerminal, Stdout, Write},
    path::Path,
    time::Duration,
};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event as TerminalEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
        supports_keyboard_enhancement,
    },
};
use fae_agent::{Ctx, Session, SessionEventData, SessionOutput};
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
use tui_markdown::{AlertKind, Options as MarkdownOptions, StyleSheet, from_str_with_options};
use unicode_width::UnicodeWidthStr;

use crate::args::ColorChoice;

const SPINNER: &[&str] = &["-", "\\", "|", "/"];
const OUTPUT_STATUS: &str = "Outputting...";
const OUTPUT_ANIMATION_STEPS: usize = OUTPUT_STATUS.len() + 3;
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
    Notice,
    System,
    Error,
}

impl MessageKind {
    fn folds_by_default(self) -> bool {
        !matches!(self, Self::User | Self::Assistant | Self::Error)
    }
}

#[derive(Debug)]
struct Message {
    kind: MessageKind,
    title: String,
    content: String,
    stream_id: Option<String>,
    expanded: bool,
    render_markdown: bool,
}

#[derive(Debug)]
struct BufferedMessage {
    kind: MessageKind,
    title: String,
    content: String,
}

impl Message {
    fn new(
        kind: MessageKind,
        title: impl Into<String>,
        content: impl Into<String>,
        stream_id: Option<String>,
    ) -> Self {
        let content = content.into();
        Self {
            kind,
            title: title.into(),
            expanded: !kind.folds_by_default() || content.is_empty(),
            content,
            stream_id,
            render_markdown: false,
        }
    }

    fn is_collapsible(&self) -> bool {
        self.kind.folds_by_default() && !self.content.is_empty()
    }

    fn shows_content(&self) -> bool {
        !self.is_collapsible() || self.expanded
    }
}

#[derive(Debug, Default)]
struct Composer {
    text: String,
    cursor: usize,
    scroll_back: u16,
}

impl Composer {
    fn insert(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.follow_cursor();
    }

    fn backspace(&mut self) {
        if let Some(previous) = self.text[..self.cursor].char_indices().next_back() {
            self.text.drain(previous.0..self.cursor);
            self.cursor = previous.0;
            self.follow_cursor();
        }
    }

    fn delete(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.text
                .drain(self.cursor..self.cursor.saturating_add(next.len_utf8()));
            self.follow_cursor();
        }
    }

    fn move_left(&mut self) {
        if let Some(previous) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = previous.0;
            self.follow_cursor();
        }
    }

    fn move_right(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
            self.follow_cursor();
        }
    }

    fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map(|position| position + 1)
            .unwrap_or(0);
        self.follow_cursor();
    }

    fn move_end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map(|offset| self.cursor + offset)
            .unwrap_or(self.text.len());
        self.follow_cursor();
    }

    fn take(&mut self) -> Option<String> {
        let value = self.text.trim().to_string();
        self.text.clear();
        self.cursor = 0;
        self.follow_cursor();
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

    fn vertical_scroll(&self, width: usize, visible_height: u16) -> u16 {
        let follow_scroll = self.max_vertical_scroll(width, visible_height);
        follow_scroll.saturating_sub(self.scroll_back.min(follow_scroll))
    }

    fn max_vertical_scroll(&self, width: usize, visible_height: u16) -> u16 {
        let (_, cursor_row) = self.visual_cursor(width);
        cursor_row.saturating_sub(visible_height.saturating_sub(1))
    }

    fn scroll_up(&mut self, lines: u16, max_scroll: u16) {
        self.scroll_back = self.scroll_back.saturating_add(lines).min(max_scroll);
    }

    fn scroll_down(&mut self, lines: u16) {
        self.scroll_back = self.scroll_back.saturating_sub(lines);
    }

    fn follow_cursor(&mut self) {
        self.scroll_back = 0;
    }
}

fn parse_steer(input: &str) -> Option<&str> {
    let content = input.strip_prefix("/steer")?;
    if !content.starts_with(char::is_whitespace) {
        return None;
    }
    let content = content.trim();
    (!content.is_empty()).then_some(content)
}

#[derive(Debug)]
pub enum PromptAction {
    Submit(String),
    Exit,
}

#[derive(Debug, PartialEq, Eq)]
enum RunningAction {
    None,
    Interrupt,
    Steer(String),
    InvalidSteer,
}

pub struct TerminalUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    events: EventStream,
    alternate_screen: bool,
    keyboard_enhancement: bool,
    copy_mode: bool,
    color: bool,
    mode: Mode,
    agent_name: String,
    user_id: Option<String>,
    model: String,
    subject: String,
    cwd: String,
    messages: Vec<Message>,
    child_streams: HashMap<String, Vec<BufferedMessage>>,
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
        agent_name: impl Into<String>,
        user_id: Option<String>,
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
        let keyboard_enhancement = supports_keyboard_enhancement().unwrap_or(false);
        let terminal = (|| -> anyhow::Result<_> {
            if keyboard_enhancement {
                execute!(
                    stdout,
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                )?;
            }
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
                if keyboard_enhancement {
                    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
                }
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
            keyboard_enhancement,
            copy_mode: false,
            color,
            mode,
            agent_name: agent_name.into(),
            user_id,
            model: model.into(),
            subject: subject.into(),
            cwd: display_cwd(),
            messages: Vec::new(),
            child_streams: HashMap::new(),
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
        self.messages.push(Message::new(
            MessageKind::User,
            self.title("You"),
            content,
            None,
        ));
        self.scroll_from_bottom = 0;
    }

    fn push_steer(&mut self, content: impl Into<String>) {
        self.finish_stream();
        self.messages.push(Message::new(
            MessageKind::User,
            self.title("Steer"),
            content,
            None,
        ));
        self.scroll_from_bottom = 0;
    }

    pub fn push_system(&mut self, content: impl Into<String>) {
        self.push_message(MessageKind::System, content);
    }

    pub fn push_notice(&mut self, content: impl Into<String>) {
        self.push_message(MessageKind::Notice, content);
    }

    pub fn push_error(&mut self, error: impl Into<String>) {
        self.finish_stream();
        let error = error.into();
        if self
            .messages
            .last()
            .is_some_and(|message| message.kind == MessageKind::Error && message.content == error)
        {
            return;
        }
        self.messages
            .push(Message::new(MessageKind::Error, "ERROR", error, None));
        self.state = RunState::Failed;
        self.scroll_from_bottom = 0;
    }

    fn push_message(&mut self, kind: MessageKind, content: impl Into<String>) {
        self.finish_stream();
        let content = content.into();
        let (title, details) = content
            .split_once('\n')
            .map_or((content.as_str(), ""), |(title, details)| (title, details));
        self.messages
            .push(Message::new(kind, self.title(title), details, None));
        self.scroll_from_bottom = 0;
    }

    pub fn clear_transcript(&mut self) {
        self.messages.clear();
        self.child_streams.clear();
        self.scroll_from_bottom = 0;
    }

    pub fn set_subject(&mut self, subject: impl Into<String>) {
        self.subject = subject.into();
    }

    pub fn workflow_result(&mut self, output: &Value) {
        self.finish_stream();
        self.messages.push(Message::new(
            MessageKind::Workflow,
            self.title("Workflow result"),
            pretty_value(output),
            None,
        ));
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
                    if self.handle_copy_mode_key(key)? {
                        continue;
                    }
                    match (key.code, key.modifiers) {
                        (KeyCode::Enter | KeyCode::Esc, _)
                        | (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL) => return Ok(()),
                        (KeyCode::PageUp, _) => self.scroll_up(PAGE_SCROLL_LINES),
                        (KeyCode::PageDown, _) => self.scroll_down(PAGE_SCROLL_LINES),
                        _ => {}
                    }
                }
                TerminalEvent::Mouse(mouse) if !self.copy_mode => self.handle_mouse(mouse),
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
                            if self.handle_copy_mode_key(key)? {
                                continue;
                            }
                            if let Some(action) = self.handle_prompt_key(key) {
                                return Ok(action);
                            }
                        }
                        TerminalEvent::Paste(content) if !self.copy_mode => {
                            self.composer.insert(&content);
                        }
                        TerminalEvent::Mouse(mouse) if !self.copy_mode => self.handle_mouse(mouse),
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
        session: &impl Session<In, SessionOutput>,
        execution: Option<&Ctx>,
        supplement: Option<fn(String) -> In>,
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
                    let terminal = self.apply_session_output(event)?;
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
                        TerminalEvent::Key(key) if key.kind == KeyEventKind::Press => {
                            if self.handle_copy_mode_key(key)? {
                                continue;
                            }
                            match self.handle_running_key(key, supplement.is_some()) {
                                RunningAction::None => {}
                                RunningAction::Interrupt => {
                                    if let Some(execution) = execution {
                                        execution.abort();
                                    }
                                    self.push_system("Interrupted");
                                    self.state = RunState::Idle;
                                    self.draw()?;
                                    return Ok(false);
                                }
                                RunningAction::Steer(content) => {
                                    let make_input =
                                        supplement.expect("steering is enabled for this session");
                                    match session.call(make_input(content.clone())).await {
                                        Ok(()) => self.push_steer(content),
                                        Err(error) => self.push_system(format!(
                                            "Steer rejected\n{error}"
                                        )),
                                    }
                                }
                                RunningAction::InvalidSteer => {
                                    self.push_system(
                                        "Invalid steer command\nUse `/steer <message>` while the agent is running.",
                                    );
                                }
                            }
                        }
                        TerminalEvent::Paste(content)
                            if !self.copy_mode && supplement.is_some() =>
                        {
                            self.composer.insert(&content);
                        }
                        TerminalEvent::Mouse(mouse) if !self.copy_mode => {
                            self.handle_mouse(mouse);
                        }
                        TerminalEvent::Resize(_, _) => {}
                        _ => {}
                    }
                }
                _ = tick.tick() => {
                    self.spinner = (self.spinner + 1) % OUTPUT_ANIMATION_STEPS;
                }
            }
        }
    }

    pub fn apply_session_output(&mut self, event: SessionOutput) -> anyhow::Result<bool> {
        let terminal = event.is_terminal();
        let nested = event.parament_plan_id.is_some();
        let source = event.runtime_id.as_deref().unwrap_or_default();
        let agent_name = event
            .agent_name
            .as_deref()
            .unwrap_or(&self.agent_name)
            .to_string();
        let run_id = format!(
            "{}:{}:{}",
            event.node_id.as_deref().unwrap_or_default(),
            event.plan_id.as_deref().unwrap_or_default(),
            agent_name
        );
        let stream_id = format!("{run_id}:{source}");
        let title = |label: &str| agent_title(&agent_name, label);
        match event.event_data()? {
            SessionEventData::ModelReasoning { content } => {
                let title = title("Thinking");
                if nested {
                    self.buffer_child_stream(run_id, MessageKind::Reasoning, title, content);
                } else {
                    self.append_stream(MessageKind::Reasoning, &title, stream_id, content);
                }
            }
            SessionEventData::ModelOutput { content } => {
                let title = title("Assistant");
                if nested {
                    self.buffer_child_stream(run_id, MessageKind::Assistant, title, content);
                } else {
                    self.append_stream(MessageKind::Assistant, &title, stream_id, content);
                }
            }
            SessionEventData::CompressionStarted {
                estimated_tokens,
                trigger_compression_size,
            } => {
                self.flush_child_streams(&run_id, None);
                self.finish_stream();
                self.messages.push(Message::new(
                    MessageKind::Notice,
                    title("Compressing context"),
                    format!(
                        "Estimated tokens: {estimated_tokens}, trigger: {trigger_compression_size}"
                    ),
                    None,
                ));
            }
            SessionEventData::CompressionCompleted { content } => {
                self.flush_child_streams(&run_id, None);
                self.finish_stream();
                self.messages.push(Message::new(
                    MessageKind::Notice,
                    title("Context compressed"),
                    content,
                    None,
                ));
            }
            SessionEventData::ToolCall { arguments, .. } => {
                self.flush_child_streams(&run_id, None);
                self.finish_stream();
                self.messages.push(Message::new(
                    MessageKind::Tool,
                    title(&format!("Called {source}")),
                    pretty_json_text(&arguments),
                    None,
                ));
            }
            SessionEventData::ToolOutput {
                output, completed, ..
            } => {
                self.flush_child_streams(&run_id, None);
                self.finish_stream();
                self.messages.push(Message::new(
                    MessageKind::Tool,
                    title(&format!(
                        "{} {}",
                        if completed { "Completed" } else { "Running" },
                        source
                    )),
                    pretty_json_text(&output),
                    None,
                ));
            }
            SessionEventData::NodeCompleted { output, finished } => {
                self.flush_child_streams(&run_id, None);
                self.finish_stream();
                self.messages.push(Message::new(
                    MessageKind::Workflow,
                    title(
                        if finished {
                            "Workflow complete".to_string()
                        } else {
                            format!("Completed {}", event.node_id.as_deref().unwrap_or(source))
                        }
                        .as_str(),
                    ),
                    if output.is_null() {
                        String::new()
                    } else {
                        pretty_value(&output)
                    },
                    None,
                ));
            }
            SessionEventData::Failed { error } => {
                self.flush_child_streams(&run_id, None);
                self.push_error(error);
            }
            SessionEventData::Custom {
                event_type,
                content,
            } => {
                self.flush_child_streams(&run_id, None);
                self.finish_stream();
                self.messages.push(Message::new(
                    MessageKind::System,
                    title(&event_type),
                    compact_value(&content),
                    None,
                ));
            }
            SessionEventData::Completed { content } => {
                let assistant_title = title("Assistant");
                if nested {
                    self.flush_child_streams(&run_id, Some((assistant_title, content)));
                } else {
                    finalize_assistant_message(&mut self.messages, &assistant_title, content);
                }
            }
            SessionEventData::TurnStarted { .. } | SessionEventData::UserInput { .. } => {}
        }
        self.scroll_from_bottom = 0;
        Ok(terminal)
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

    fn buffer_child_stream(
        &mut self,
        run_id: String,
        kind: MessageKind,
        title: String,
        content: String,
    ) {
        buffer_child_stream_message(&mut self.child_streams, run_id, kind, title, content);
    }

    fn flush_child_streams(&mut self, run_id: &str, completed_assistant: Option<(String, String)>) {
        flush_child_stream_messages(
            &mut self.child_streams,
            &mut self.messages,
            run_id,
            completed_assistant,
        );
    }

    fn finish_stream(&mut self) {
        if let Some(message) = self.messages.last_mut() {
            message.stream_id = None;
        }
    }

    fn title(&self, label: &str) -> String {
        agent_title(&self.agent_name, label)
    }

    fn handle_copy_mode_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        if self.copy_mode {
            if matches!(key.code, KeyCode::F(2) | KeyCode::Esc) {
                self.set_copy_mode(false)?;
            }
            return Ok(true);
        }
        if key.code == KeyCode::F(2) {
            self.set_copy_mode(true)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn set_copy_mode(&mut self, enabled: bool) -> anyhow::Result<()> {
        if self.copy_mode == enabled {
            return Ok(());
        }
        if enabled {
            self.copy_mode = true;
            self.draw_current()?;
            if let Err(error) = execute!(self.terminal.backend_mut(), DisableMouseCapture) {
                self.copy_mode = false;
                return Err(error.into());
            }
        } else {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
            self.copy_mode = false;
        }
        Ok(())
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
                    self.composer.follow_cursor();
                    return;
                }
                None => return,
            }
        };
        self.history_index = Some(next);
        self.composer.text = self.input_history[next].clone();
        self.composer.cursor = self.composer.text.len();
        self.composer.follow_cursor();
    }

    fn handle_running_key(&mut self, key: KeyEvent, can_steer: bool) -> RunningAction {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => RunningAction::Interrupt,
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                RunningAction::Interrupt
            }
            (KeyCode::PageUp, _) => {
                self.scroll_up(PAGE_SCROLL_LINES);
                RunningAction::None
            }
            (KeyCode::PageDown, _) => {
                self.scroll_down(PAGE_SCROLL_LINES);
                RunningAction::None
            }
            _ if !can_steer => RunningAction::None,
            (KeyCode::Char('j'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.insert("\n");
                RunningAction::None
            }
            (KeyCode::Enter, modifiers)
                if modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.composer.insert("\n");
                RunningAction::None
            }
            (KeyCode::Enter, _) => self.composer.take().map_or(RunningAction::None, |input| {
                parse_steer(&input)
                    .map(ToOwned::to_owned)
                    .map_or(RunningAction::InvalidSteer, RunningAction::Steer)
            }),
            (KeyCode::Char(character), modifiers)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.composer.insert(&character.to_string());
                RunningAction::None
            }
            (KeyCode::Backspace, _) => {
                self.composer.backspace();
                RunningAction::None
            }
            (KeyCode::Delete, _) => {
                self.composer.delete();
                RunningAction::None
            }
            (KeyCode::Left, _) => {
                self.composer.move_left();
                RunningAction::None
            }
            (KeyCode::Right, _) => {
                self.composer.move_right();
                RunningAction::None
            }
            (KeyCode::Home, _) => {
                self.composer.move_home();
                RunningAction::None
            }
            (KeyCode::End, _) => {
                self.composer.move_end();
                RunningAction::None
            }
            _ => RunningAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let frame_area = self.terminal.get_frame().area();
        let composer_area = frame_areas(frame_area, &self.composer)[2];
        let composer_width = composer_area.width.saturating_sub(4).max(1) as usize;
        let composer_height = composer_area.height.saturating_sub(2).max(1);
        let max_composer_scroll = self
            .composer
            .max_vertical_scroll(composer_width, composer_height);
        let composer_is_editable = matches!(self.state, RunState::Idle | RunState::Failed)
            || self.state == RunState::Running && self.mode == Mode::Agent;
        let over_scrollable_composer = composer_is_editable
            && max_composer_scroll > 0
            && mouse.row >= composer_area.y
            && mouse.row < composer_area.bottom();
        match mouse.kind {
            MouseEventKind::ScrollUp if over_scrollable_composer => {
                self.composer
                    .scroll_up(MOUSE_SCROLL_LINES, max_composer_scroll);
            }
            MouseEventKind::ScrollDown if over_scrollable_composer => {
                self.composer.scroll_down(MOUSE_SCROLL_LINES);
            }
            MouseEventKind::ScrollUp => self.scroll_up(MOUSE_SCROLL_LINES),
            MouseEventKind::ScrollDown => self.scroll_down(MOUSE_SCROLL_LINES),
            MouseEventKind::Down(MouseButton::Left) => {
                self.toggle_message_at(mouse.column, mouse.row);
            }
            _ => {}
        }
    }

    fn toggle_message_at(&mut self, column: u16, row: u16) {
        let frame_area = self.terminal.get_frame().area();
        let areas = frame_areas(frame_area, &self.composer);
        let transcript_area = areas[1];
        let Some(index) = message_at_position(
            &self.messages,
            transcript_area,
            self.scroll_from_bottom,
            column,
            row,
        ) else {
            return;
        };
        let message = &mut self.messages[index];
        if message.is_collapsible() {
            message.expanded = !message.expanded;
        }
    }

    fn scroll_up(&mut self, lines: u16) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(lines);
    }

    fn scroll_down(&mut self, lines: u16) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(lines);
    }

    fn draw(&mut self) -> anyhow::Result<()> {
        if self.copy_mode {
            return Ok(());
        }
        self.draw_current()
    }

    fn draw_current(&mut self) -> anyhow::Result<()> {
        let color = self.color;
        let mode = self.mode;
        let agent_name = self.agent_name.clone();
        let user_id = self.user_id.clone();
        let model = self.model.clone();
        let subject = self.subject.clone();
        let cwd = self.cwd.clone();
        let state = self.state;
        let spinner = self.spinner;
        let messages = &self.messages;
        let composer = &self.composer;
        let scroll_from_bottom = self.scroll_from_bottom;
        let copy_mode = self.copy_mode;

        self.terminal.draw(|frame| {
            draw_frame(
                frame,
                ViewModel {
                    color,
                    mode,
                    agent_name: &agent_name,
                    user_id: user_id.as_deref(),
                    model: &model,
                    subject: &subject,
                    cwd: &cwd,
                    state,
                    spinner,
                    messages,
                    composer,
                    scroll_from_bottom,
                    copy_mode,
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
        if self.keyboard_enhancement {
            let _ = execute!(self.terminal.backend_mut(), PopKeyboardEnhancementFlags);
        }
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
    agent_name: &'a str,
    user_id: Option<&'a str>,
    model: &'a str,
    subject: &'a str,
    cwd: &'a str,
    state: RunState,
    spinner: usize,
    messages: &'a [Message],
    composer: &'a Composer,
    scroll_from_bottom: u16,
    copy_mode: bool,
}

fn frame_areas(area: Rect, composer: &Composer) -> [Rect; 4] {
    let composer_width = area.width.saturating_sub(4).max(1) as usize;
    let composer_height = (composer.visual_lines(composer_width) as u16 + 2).clamp(3, 8);
    Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(area)
}

fn draw_frame(frame: &mut Frame<'_>, view: ViewModel<'_>) {
    let areas = frame_areas(frame.area(), view.composer);

    draw_header(frame, areas[0], &view);
    draw_transcript(frame, areas[1], &view);
    draw_composer(frame, areas[2], &view);
    draw_footer(frame, areas[3], &view);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, view: &ViewModel<'_>) {
    let accent = color(view.color, Color::Cyan);
    let mut title = vec![Span::styled(
        " FAE ",
        Style::default().fg(Color::Black).bg(accent).bold(),
    )];
    match view.mode {
        Mode::Agent => {
            let label_style = Style::default().fg(color(view.color, Color::DarkGray));
            title.extend([
                Span::styled(" AGENT:", label_style),
                Span::raw(view.agent_name),
                Span::styled(" USER:", label_style),
                Span::raw(view.user_id.unwrap_or_default()),
                Span::styled(" SESSION:", label_style),
                Span::raw(view.subject),
            ]);
        }
        Mode::Workflow => {
            title.extend([
                Span::styled(" Workflow", Style::default().fg(accent).bold()),
                Span::raw(format!(" {}", view.subject)),
            ]);
        }
    }
    let title = Line::from(title);
    let details = Line::from(vec![
        Span::styled(
            " model ",
            Style::default().fg(color(view.color, Color::DarkGray)),
        ),
        Span::raw(view.model),
        Span::styled(
            " cwd ",
            Style::default().fg(color(view.color, Color::DarkGray)),
        ),
        Span::raw(view.cwd),
    ]);
    frame.render_widget(Paragraph::new(vec![title, details]), area);
}

fn draw_transcript(frame: &mut Frame<'_>, area: Rect, view: &ViewModel<'_>) {
    let text = transcript_text(view.messages, view.color, view.spinner);
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .block(Block::default().padding(Padding::horizontal(1)));
    let width = area.width.saturating_sub(2);
    let total_lines = paragraph.line_count(width).min(u16::MAX as usize) as u16;
    let max_scroll = total_lines.saturating_sub(area.height);
    let scroll = max_scroll.saturating_sub(view.scroll_from_bottom.min(max_scroll));
    frame.render_widget(paragraph.scroll((scroll, 0)), area);
}

fn draw_composer(frame: &mut Frame<'_>, area: Rect, view: &ViewModel<'_>) {
    let accent = color(view.color, Color::Cyan);
    let editable = matches!(view.state, RunState::Idle | RunState::Failed)
        || view.state == RunState::Running && view.mode == Mode::Agent;
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
            if view.mode == Mode::Agent {
                " Steer "
            } else {
                " Working "
            },
            color(view.color, Color::Yellow),
            if view.mode == Mode::Agent {
                if view.composer.text.is_empty() {
                    format!(
                        "{} Running... /steer <message>",
                        SPINNER[view.spinner % SPINNER.len()]
                    )
                } else {
                    view.composer.text.clone()
                }
            } else {
                format!(
                    "{} Running... Esc to interrupt",
                    SPINNER[view.spinner % SPINNER.len()]
                )
            },
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
    let shows_placeholder = view.composer.text.is_empty()
        && (view.state == RunState::Idle
            || view.state == RunState::Running && view.mode == Mode::Agent);
    let content_style = if shows_placeholder {
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
    let inner_width = area.width.saturating_sub(4).max(1) as usize;
    let inner_height = area.height.saturating_sub(2).max(1);
    let scroll = editable
        .then(|| view.composer.vertical_scroll(inner_width, inner_height))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(content)
            .style(content_style)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(block),
        area,
    );

    if !view.copy_mode && editable {
        let (cursor_x, cursor_y) = view.composer.visual_cursor(inner_width);
        if cursor_y >= scroll && cursor_y < scroll.saturating_add(inner_height) {
            frame.set_cursor_position(Position::new(
                area.x + 2 + cursor_x,
                area.y + 1 + cursor_y - scroll,
            ));
        }
    }
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, view: &ViewModel<'_>) {
    let hint = if view.copy_mode {
        " COPY MODE  Drag to select  Use terminal copy shortcut  F2/Esc resume "
    } else {
        match view.state {
            RunState::Running if view.mode == Mode::Agent => {
                " Enter steer  Esc interrupt  F2 copy  Wheel/PgUp/PgDn scroll "
            }
            RunState::Running => " Esc interrupt  F2 copy  Wheel/PgUp/PgDn scroll ",
            RunState::Completed => " Enter close  F2 copy  Wheel/PgUp/PgDn scroll ",
            _ => " Enter send  Shift+Enter newline  F2 copy  Wheel/PgUp/PgDn scroll  Ctrl+C quit ",
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(color(view.color, Color::DarkGray)),
        ))),
        area,
    );
}

fn transcript_text(messages: &[Message], use_color: bool, pulse: usize) -> Text<'static> {
    let mut lines = Vec::new();
    if messages.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Start by entering a task below.",
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
            MessageKind::Notice => ("-", Color::Blue, Modifier::BOLD),
            MessageKind::System => ("-", Color::DarkGray, Modifier::DIM),
            MessageKind::Error => ("!", Color::Red, Modifier::BOLD),
        };
        let heading = if message.title.is_empty() {
            marker.to_string()
        } else {
            format!("{marker} {}", message.title)
        };
        let disclosure = if message.is_collapsible() {
            if message.expanded { "[-] " } else { "[+] " }
        } else {
            ""
        };
        let mut heading_spans = vec![Span::styled(
            format!("{disclosure}{heading}"),
            Style::default()
                .fg(color(use_color, marker_color))
                .add_modifier(modifier),
        )];
        if message.stream_id.is_some() {
            heading_spans.extend(output_status_spans(use_color, pulse));
        }
        lines.push(Line::from(heading_spans));
        if message.shows_content() && !message.content.is_empty() {
            lines.extend(message_content_lines(message, use_color));
        }
    }
    Text::from(lines)
}

#[derive(Clone, Copy)]
struct AssistantMarkdownStyle {
    use_color: bool,
}

impl StyleSheet for AssistantMarkdownStyle {
    fn heading(&self, level: u8) -> Style {
        let style = Style::default()
            .fg(color(self.use_color, Color::Green))
            .add_modifier(Modifier::BOLD);
        if level == 1 {
            style.add_modifier(Modifier::UNDERLINED)
        } else {
            style
        }
    }

    fn code(&self) -> Style {
        if self.use_color {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(color(self.use_color, Color::Cyan))
            .add_modifier(Modifier::UNDERLINED)
    }

    fn blockquote(&self) -> Style {
        Style::default()
            .fg(color(self.use_color, Color::DarkGray))
            .add_modifier(Modifier::ITALIC)
    }

    fn heading_meta(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    fn metadata_block(&self) -> Style {
        Style::default().fg(color(self.use_color, Color::Yellow))
    }

    fn html(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    fn math_inline(&self) -> Style {
        Style::default()
            .fg(color(self.use_color, Color::Magenta))
            .add_modifier(Modifier::ITALIC)
    }

    fn math_display(&self) -> Style {
        Style::default().fg(color(self.use_color, Color::Magenta))
    }

    fn alert(&self, kind: AlertKind) -> Style {
        let alert_color = match kind {
            AlertKind::Note => Color::Blue,
            AlertKind::Tip => Color::Green,
            AlertKind::Important => Color::Magenta,
            AlertKind::Warning => Color::Yellow,
            AlertKind::Caution => Color::Red,
        };
        Style::default().fg(color(self.use_color, alert_color))
    }

    fn alert_icon(&self, _kind: AlertKind) -> &str {
        "!"
    }

    fn table_header(&self) -> Style {
        Style::default()
            .fg(color(self.use_color, Color::Cyan))
            .add_modifier(Modifier::BOLD)
    }

    fn table_border(&self) -> Style {
        Style::default().fg(color(self.use_color, Color::DarkGray))
    }
}

fn message_content_lines(message: &Message, use_color: bool) -> Vec<Line<'static>> {
    if message.kind == MessageKind::Assistant && message.render_markdown {
        let options = MarkdownOptions::new(AssistantMarkdownStyle { use_color });
        let mut text = from_str_with_options(&message.content, &options);
        for line in &mut text.lines {
            line.spans.insert(0, Span::raw(" "));
        }
        text.lines
            .into_iter()
            .map(|line| Line {
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| Span::styled(span.content.into_owned(), span.style))
                    .collect(),
                style: line.style,
                alignment: line.alignment,
            })
            .collect()
    } else {
        message
            .content
            .lines()
            .map(|content_line| Line::from(format!(" {content_line}")))
            .collect()
    }
}

fn output_status_spans(use_color: bool, tick: usize) -> Vec<Span<'static>> {
    let head = tick % OUTPUT_ANIMATION_STEPS;
    let mut spans = Vec::with_capacity(OUTPUT_STATUS.len() + 1);
    spans.push(Span::raw(" "));
    for (index, character) in OUTPUT_STATUS.chars().enumerate() {
        let style = match head.checked_sub(index) {
            Some(0) => Style::default()
                .fg(color(use_color, Color::Yellow))
                .add_modifier(Modifier::BOLD),
            Some(1) => Style::default().fg(color(use_color, Color::White)),
            Some(2) => Style::default().fg(color(use_color, Color::Gray)),
            _ => Style::default()
                .fg(color(use_color, Color::DarkGray))
                .add_modifier(Modifier::DIM),
        };
        spans.push(Span::styled(character.to_string(), style));
    }
    spans
}

fn message_at_position(
    messages: &[Message],
    area: Rect,
    scroll_from_bottom: u16,
    column: u16,
    row: u16,
) -> Option<usize> {
    if row < area.top()
        || row >= area.bottom()
        || column <= area.left()
        || column >= area.right().saturating_sub(1)
    {
        return None;
    }

    let width = area.width.saturating_sub(2);
    let mut heading_ranges = Vec::with_capacity(messages.len());
    let mut total_lines = 0u16;
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            total_lines = total_lines.saturating_add(1);
        }
        let heading_start = total_lines;
        total_lines =
            total_lines.saturating_add(wrapped_line_count(&message_heading(message, true), width));
        heading_ranges.push((heading_start..total_lines, index));

        if message.shows_content() {
            for line in message_content_lines(message, false) {
                total_lines =
                    total_lines.saturating_add(wrapped_line_count(&line.to_string(), width));
            }
        }
    }

    let max_scroll = total_lines.saturating_sub(area.height);
    let scroll = max_scroll.saturating_sub(scroll_from_bottom.min(max_scroll));
    let clicked_line = scroll.saturating_add(row.saturating_sub(area.y));
    heading_ranges
        .into_iter()
        .find_map(|(range, index)| range.contains(&clicked_line).then_some(index))
}

fn wrapped_line_count(line: &str, width: u16) -> u16 {
    Paragraph::new(line)
        .wrap(Wrap { trim: false })
        .line_count(width)
        .min(u16::MAX as usize) as u16
}

fn message_heading(message: &Message, include_output_status: bool) -> String {
    let marker = match message.kind {
        MessageKind::User => ">",
        MessageKind::Assistant => "*",
        MessageKind::Reasoning => "-",
        MessageKind::Tool => "$",
        MessageKind::Workflow => "+",
        MessageKind::Notice => "-",
        MessageKind::System => "-",
        MessageKind::Error => "!",
    };
    let disclosure = if message.is_collapsible() {
        if message.expanded { "[-] " } else { "[+] " }
    } else {
        ""
    };
    let title = if message.title.is_empty() {
        marker.to_string()
    } else {
        format!("{marker} {}", message.title)
    };
    let output_status = if include_output_status && message.stream_id.is_some() {
        concat!(" ", "Outputting...")
    } else {
        ""
    };
    format!("{disclosure}{title}{output_status}")
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
                MessageKind::Notice => "-",
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
                        .map(|line| format!(" {line}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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
        messages.push(Message::new(kind, title, content, Some(stream_id)));
    }
}

fn buffer_child_stream_message(
    child_streams: &mut HashMap<String, Vec<BufferedMessage>>,
    run_id: String,
    kind: MessageKind,
    title: String,
    content: String,
) {
    let streams = child_streams.entry(run_id).or_default();
    if let Some(message) = streams.last_mut()
        && message.kind == kind
    {
        message.content.push_str(&content);
    } else {
        streams.push(BufferedMessage {
            kind,
            title,
            content,
        });
    }
}

fn flush_child_stream_messages(
    child_streams: &mut HashMap<String, Vec<BufferedMessage>>,
    messages: &mut Vec<Message>,
    run_id: &str,
    completed_assistant: Option<(String, String)>,
) {
    for buffered in child_streams.remove(run_id).unwrap_or_default() {
        if completed_assistant.is_some() && buffered.kind == MessageKind::Assistant {
            continue;
        }
        let mut message = Message::new(buffered.kind, buffered.title, buffered.content, None);
        message.render_markdown = buffered.kind == MessageKind::Assistant;
        messages.push(message);
    }
    if let Some((title, content)) = completed_assistant
        && !content.is_empty()
    {
        let mut message = Message::new(MessageKind::Assistant, title, content, None);
        message.render_markdown = true;
        messages.push(message);
    }
}

fn agent_title(agent_name: &str, title: &str) -> String {
    format!("{agent_name}: {title}")
}

fn finalize_assistant_message(messages: &mut Vec<Message>, title: &str, content: String) {
    if let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| message.kind == MessageKind::Assistant && message.title == title)
    {
        message.stream_id = None;
        if !content.is_empty() && message.content != content {
            message.content = content;
        }
        message.render_markdown = true;
    } else if !content.is_empty() {
        let mut message = Message::new(MessageKind::Assistant, title, content, None);
        message.render_markdown = true;
        messages.push(message);
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
    use fae_agent::{SessionEvent, SessionEventData};
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
    fn composer_scroll_follows_cursor_and_allows_scrolling_back() {
        let mut composer = Composer::default();
        composer.insert("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight");

        assert_eq!(composer.vertical_scroll(40, 6), 2);

        composer.scroll_up(1, 2);
        assert_eq!(composer.vertical_scroll(40, 6), 1);

        composer.insert("!");
        assert_eq!(composer.vertical_scroll(40, 6), 2);
    }

    #[test]
    fn composer_renders_new_lines_after_reaching_max_height() {
        let backend = TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut composer = Composer::default();
        composer.insert("FIRST\nsecond\nthird\nfourth\nfifth\nsixth\nseventh\nLAST");

        terminal
            .draw(|frame| {
                let area = frame_areas(frame.area(), &composer)[2];
                draw_composer(
                    frame,
                    area,
                    &ViewModel {
                        color: false,
                        mode: Mode::Agent,
                        agent_name: "agent",
                        user_id: Some("user"),
                        model: "model",
                        subject: "session",
                        cwd: "/workspace",
                        state: RunState::Idle,
                        spinner: 0,
                        messages: &[],
                        composer: &composer,
                        scroll_from_bottom: 0,
                        copy_mode: false,
                    },
                );
            })
            .unwrap();

        let content = terminal.backend().to_string();
        assert!(content.contains("LAST"));
        assert!(!content.contains("FIRST"));

        composer.scroll_up(MOUSE_SCROLL_LINES, 2);
        terminal
            .draw(|frame| {
                let area = frame_areas(frame.area(), &composer)[2];
                draw_composer(
                    frame,
                    area,
                    &ViewModel {
                        color: false,
                        mode: Mode::Agent,
                        agent_name: "agent",
                        user_id: Some("user"),
                        model: "model",
                        subject: "session",
                        cwd: "/workspace",
                        state: RunState::Idle,
                        spinner: 0,
                        messages: &[],
                        composer: &composer,
                        scroll_from_bottom: 0,
                        copy_mode: false,
                    },
                );
            })
            .unwrap();

        let content = terminal.backend().to_string();
        assert!(content.contains("FIRST"));
        assert!(!content.contains("LAST"));
    }

    #[test]
    fn steer_command_requires_fixed_prefix_and_content() {
        assert_eq!(
            parse_steer("/steer  use the new constraint"),
            Some("use the new constraint")
        );
        assert_eq!(parse_steer("/steer\nsecond line"), Some("second line"));
        assert_eq!(parse_steer("/steer"), None);
        assert_eq!(parse_steer("/steering elsewhere"), None);
        assert_eq!(parse_steer(" /steer too late"), None);
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
    fn child_assistant_streams_are_buffered_and_isolated_by_run() {
        let mut child_streams = HashMap::new();
        let mut messages = Vec::new();
        buffer_child_stream_message(
            &mut child_streams,
            "researcher:1".to_string(),
            MessageKind::Assistant,
            "researcher: Assistant".to_string(),
            "par".to_string(),
        );
        buffer_child_stream_message(
            &mut child_streams,
            "reviewer:1".to_string(),
            MessageKind::Assistant,
            "reviewer: Assistant".to_string(),
            "dra".to_string(),
        );
        buffer_child_stream_message(
            &mut child_streams,
            "researcher:1".to_string(),
            MessageKind::Assistant,
            "researcher: Assistant".to_string(),
            "tial".to_string(),
        );

        assert!(messages.is_empty());
        flush_child_stream_messages(
            &mut child_streams,
            &mut messages,
            "reviewer:1",
            Some(("reviewer: Assistant".to_string(), "draft".to_string())),
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].title, "reviewer: Assistant");
        assert_eq!(messages[0].content, "draft");
        assert!(child_streams.contains_key("researcher:1"));

        flush_child_stream_messages(
            &mut child_streams,
            &mut messages,
            "researcher:1",
            Some(("researcher: Assistant".to_string(), "partial".to_string())),
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].title, "researcher: Assistant");
        assert_eq!(messages[1].content, "partial");
        assert!(messages.iter().all(|message| message.stream_id.is_none()));
    }

    #[test]
    fn final_assistant_output_is_rendered_as_markdown() {
        let mut messages = Vec::new();
        append_stream_message(
            &mut messages,
            MessageKind::Assistant,
            "Assistant",
            "stream".to_string(),
            "**partial**".to_string(),
        );

        let streaming = message_content_lines(&messages[0], false);
        assert_eq!(streaming[0].to_string(), " **partial**");

        finalize_assistant_message(&mut messages, "Assistant", "**final**".to_string());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "**final**");
        assert_eq!(messages[0].stream_id, None);
        assert!(messages[0].render_markdown);
        let rendered = message_content_lines(&messages[0], false);
        assert_eq!(rendered[0].to_string(), " final");
        assert!(
            rendered[0].spans[1]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn final_assistant_output_renders_markdown_blocks() {
        let mut messages = vec![Message::new(
            MessageKind::Assistant,
            "Assistant",
            "",
            Some("stream".to_string()),
        )];
        finalize_assistant_message(
            &mut messages,
            "Assistant",
            "# Result\n\n- first\n- second\n\n`cargo check`".to_string(),
        );

        let rendered = message_content_lines(&messages[0], true);
        let content = rendered
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(content.contains("# Result"));
        assert!(content.contains("- first"));
        assert!(content.contains("cargo check"));
        assert!(
            rendered
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg == Some(Color::Yellow))
        );
    }

    #[test]
    fn non_model_details_are_collapsed_by_default() {
        let tool = Message::new(MessageKind::Tool, "Called shell", "{\"cmd\":\"pwd\"}", None);
        let assistant = Message::new(MessageKind::Assistant, "Assistant", "Answer", None);

        assert!(tool.is_collapsible());
        assert!(!tool.expanded);
        assert!(!tool.shows_content());
        assert!(assistant.expanded);
        assert!(assistant.shows_content());
    }

    #[test]
    fn notice_messages_use_blue_headings() {
        let messages = vec![Message::new(
            MessageKind::Notice,
            "Context compressed",
            "",
            None,
        )];

        let rendered = transcript_text(&messages, true, 0);

        assert_eq!(rendered.lines[0].spans[0].style.fg, Some(Color::Blue));
    }

    #[test]
    fn collapsed_details_are_hidden_and_output_status_is_visible() {
        let messages = vec![Message::new(
            MessageKind::Reasoning,
            "Thinking",
            "private details",
            Some("stream".to_string()),
        )];

        let text = transcript_text(&messages, false, 0);
        let rendered = text
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("[+] - Thinking Outputting..."));
        assert!(!rendered.contains("private details"));
    }

    #[test]
    fn output_status_highlight_moves_from_left_to_right() {
        let first_frame = output_status_spans(true, 0);
        let fourth_frame = output_status_spans(true, 3);

        assert_eq!(first_frame[1].style.fg, Some(Color::Yellow));
        assert_eq!(fourth_frame[1].style.fg, Some(Color::DarkGray));
        assert_eq!(fourth_frame[4].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn clicking_a_collapsed_heading_finds_its_message() {
        let messages = vec![
            Message::new(MessageKind::User, "You", "Question", None),
            Message::new(MessageKind::Tool, "Called shell", "details", None),
        ];
        let area = Rect::new(0, 3, 40, 6);

        assert_eq!(message_at_position(&messages, area, 0, 1, 6), Some(1));
        assert_eq!(message_at_position(&messages, area, 0, 1, 5), None);
    }

    #[test]
    fn compact_layout_renders_without_overlap() {
        let backend = TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let messages = vec![Message::new(
            MessageKind::Assistant,
            "Assistant",
            "A response that remains visible above the composer.",
            None,
        )];
        let composer = Composer::default();

        terminal
            .draw(|frame| {
                draw_frame(
                    frame,
                    ViewModel {
                        color: false,
                        mode: Mode::Agent,
                        agent_name: "test-agent",
                        user_id: Some("test-user"),
                        model: "test-model",
                        subject: "test-session",
                        cwd: "/workspace",
                        state: RunState::Idle,
                        spinner: 0,
                        messages: &messages,
                        composer: &composer,
                        scroll_from_bottom: 0,
                        copy_mode: false,
                    },
                )
            })
            .unwrap();

        let content = terminal.backend().to_string();
        assert!(content.contains("FAE  AGENT:test-agent USER:test-user SESSION:test-session"));
        assert!(content.contains("A response that remains visible"));
        assert!(content.contains("Message"));
        assert!(content.contains("Enter send"));
        assert!(content.contains("F2 copy"));
    }

    #[test]
    fn transcript_scrolls_to_last_word_wrapped_line() {
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let messages = vec![Message::new(
            MessageKind::Assistant,
            "Assistant",
            "1234567890 1234567890 FINAL_END",
            None,
        )];
        let composer = Composer::default();

        terminal
            .draw(|frame| {
                draw_frame(
                    frame,
                    ViewModel {
                        color: false,
                        mode: Mode::Agent,
                        agent_name: "agent",
                        user_id: Some("user"),
                        model: "model",
                        subject: "session",
                        cwd: "/workspace",
                        state: RunState::Idle,
                        spinner: 0,
                        messages: &messages,
                        composer: &composer,
                        scroll_from_bottom: 0,
                        copy_mode: false,
                    },
                )
            })
            .unwrap();

        assert!(terminal.backend().to_string().contains("FINAL_END"));
    }

    #[test]
    fn plain_transcript_preserves_visible_conversation() {
        let messages = vec![
            Message::new(MessageKind::User, "You", "Question", None),
            Message::new(
                MessageKind::Assistant,
                "Assistant",
                "First line\nSecond line",
                None,
            ),
        ];

        assert_eq!(
            plain_transcript(&messages),
            "> You\n Question\n\n* Assistant\n First line\n Second line"
        );
    }

    #[test]
    fn error_is_expanded_and_preserved_in_plain_transcript() {
        let message = Message::new(
            MessageKind::Error,
            "ERROR",
            "request failed\nconnection reset",
            None,
        );

        assert!(message.expanded);
        assert!(message.shows_content());
        assert_eq!(
            plain_transcript(&[message]),
            "! ERROR\n request failed\n connection reset"
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
