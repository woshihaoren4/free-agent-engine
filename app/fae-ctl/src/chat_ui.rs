use crossterm::{
    ExecutableCommand,
    cursor::{SetCursorStyle, Show},
    event::{
        self, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
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
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::pin::Pin;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio_stream::StreamExt;
use tui_input::{
    Input, InputRequest,
    backend::crossterm::{EventHandler, to_input_request},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

type Tui = Terminal<CrosstermBackend<File>>;
const INPUT_PLACEHOLDER: &str = "Type a message...";
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(120);
const STREAM_POLL_INTERVAL: Duration = Duration::from_millis(30);

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
        let (guard, terminal_file) = TerminalGuard::enter()?;
        let mut output_capture = OutputCapture::start()?;
        let backend = CrosstermBackend::new(terminal_file);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        let mut outcome = match self.ws.get_agent(&self.agent_name).await {
            Ok(agent) => {
                let agent = agent.on_info().await;
                self.model_name = agent.model();
                self.run_app(&mut terminal, &mut output_capture).await
            }
            Err(e) => {
                let mut state = ChatState::default();
                state.push_error(format!("Failed to load agent: {e:?}"));
                ChatRunOutcome::err(state, e)
            }
        };
        for output in output_capture.finish() {
            outcome.state.push_output(&output);
        }

        let _ = terminal.show_cursor();
        drop(terminal);
        drop(guard);
        replay_scrollback(&outcome.state, &self.agent_name);

        outcome.result
    }

    async fn run_app(
        &mut self,
        terminal: &mut Tui,
        output_capture: &mut OutputCapture,
    ) -> ChatRunOutcome {
        let mut state = ChatState::default();
        let session_config = Self::new_session_config();
        let mut session_id = session_config.id.clone();
        let mut user_id = session_config.user_id.clone();
        let mut session = match self
            .ws
            .session_call_stream::<_, Record, Record>(&self.agent_name, session_config)
            .await
        {
            Ok(session) => session,
            Err(e) => {
                state.push_error(format!("Failed to create session: {e:?}"));
                return ChatRunOutcome::err(state, e);
            }
        };

        let mut stream_active = false;
        let mut current_stream: Option<Pin<Box<dyn tokio_stream::Stream<Item = Record> + Send>>> =
            None;
        let mut pending_alt_prefix_until = None;
        let mut needs_draw = true;

        loop {
            if output_capture.drain_to_state(&mut state) {
                needs_draw = true;
            }
            if needs_draw {
                if let Err(e) = terminal.draw(|frame| self.render(frame, &mut state)) {
                    state.push_error(format!("Failed to draw UI: {e:?}"));
                    return ChatRunOutcome::err(state, e);
                }
                needs_draw = false;
            }

            match event::poll(if stream_active {
                STREAM_POLL_INTERVAL
            } else {
                IDLE_POLL_INTERVAL
            }) {
                Ok(true) => {
                    let event = match event::read() {
                        Ok(event) => event,
                        Err(e) => {
                            state.push_error(format!("Failed to read terminal event: {e:?}"));
                            return ChatRunOutcome::err(state, e);
                        }
                    };
                    if matches!(&event, Event::Resize(_, _)) {
                        needs_draw = true;
                    }
                    if let Event::Key(key) = event {
                        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                            continue;
                        }
                        needs_draw = true;

                        let now = Instant::now();
                        let alt_prefix_active = pending_alt_prefix_until
                            .take()
                            .is_some_and(|deadline| now <= deadline);

                        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
                            pending_alt_prefix_until = Some(now + Duration::from_millis(100));
                            continue;
                        }

                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if !stream_active {
                                    return ChatRunOutcome::ok(state);
                                }
                                stream_active = false;
                                current_stream = None;
                                state.current_title.clear();
                                state.push_system("Session aborted. Starting a new session.");
                                let session_config = Self::new_session_config();
                                session_id = session_config.id.clone();
                                user_id = session_config.user_id.clone();
                                session = match self
                                    .ws
                                    .session_call_stream::<_, Record, Record>(
                                        &self.agent_name,
                                        session_config,
                                    )
                                    .await
                                {
                                    Ok(session) => session,
                                    Err(e) => {
                                        state.push_error(format!(
                                            "Failed to create new session: {e:?}"
                                        ));
                                        return ChatRunOutcome::err(state, e);
                                    }
                                };
                                state.input.reset();
                                state.status = "Ready".to_string();
                            }
                            KeyCode::Enter
                                if key.modifiers.contains(KeyModifiers::ALT)
                                    || (alt_prefix_active && key.modifiers.is_empty()) =>
                            {
                                state.input.handle(InputRequest::InsertChar('\n'));
                            }
                            KeyCode::PageUp => state.scroll_up(),
                            KeyCode::PageDown => state.scroll_down(),
                            KeyCode::Enter if key.modifiers.is_empty() => {
                                let raw = state.input.value().to_string();
                                let val = raw.trim();
                                if let Some(command) = ChatCommand::parse(val) {
                                    state.input.reset();
                                    match self
                                        .handle_command(command, &mut state, &user_id, &session_id)
                                        .await
                                    {
                                        CommandAction::Continue => {}
                                        CommandAction::Exit => return ChatRunOutcome::ok(state),
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
                                        match session
                                            .call_stream(Record::from_user_input(msg))
                                            .await
                                        {
                                            Ok(s) => {
                                                current_stream = Some(Pin::from(s));
                                                stream_active = true;
                                                state.current_title = "Waiting".to_string();
                                                state.status = "Waiting".to_string();
                                            }
                                            Err(e) => {
                                                state.push_error(format!(
                                                    "Failed to send chat: {e:?}"
                                                ));
                                                state.status = "Ready".to_string();
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::Enter => {}
                            _ => {
                                if to_input_request(&Event::Key(key)).is_some() {
                                    state.input.handle_event(&Event::Key(key));
                                }
                            }
                        }
                    }
                }
                Ok(false) => {}
                Err(e) => {
                    state.push_error(format!("Failed to poll terminal event: {e:?}"));
                    return ChatRunOutcome::err(state, e);
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
                needs_draw = true;
            }
            if output_capture.drain_to_state(&mut state) {
                needs_draw = true;
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

        let height = wrapped_height(&lines, area.width);
        let max_scroll = height
            .saturating_sub(area.height as usize)
            .min(u16::MAX as usize) as u16;
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
                INPUT_PLACEHOLDER,
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

    async fn handle_command(
        &self,
        command: ChatCommand,
        state: &mut ChatState,
        user_id: &str,
        session_id: &str,
    ) -> CommandAction {
        match command {
            ChatCommand::Exit => CommandAction::Exit,
            ChatCommand::Reset => {
                match self
                    .ws
                    .session_reset(&self.agent_name, user_id, session_id)
                    .await
                {
                    Ok(()) => state.push_system("Session reset successfully."),
                    Err(e) => state.push_error(format!("Failed to reset session: {e:?}")),
                }
                CommandAction::Continue
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ChatCommand {
    Exit,
    Reset,
}

impl ChatCommand {
    fn parse(input: &str) -> Option<Self> {
        match input.split_whitespace().next()? {
            "/exit" => Some(Self::Exit),
            "/reset" => Some(Self::Reset),
            _ => None,
        }
    }
}

enum CommandAction {
    Continue,
    Exit,
}

struct TerminalGuard {
    tty: File,
}

impl TerminalGuard {
    fn enter() -> io::Result<(Self, File)> {
        let mut tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        let terminal_file = tty.try_clone()?;
        enable_raw_mode()?;
        tty.execute(EnterAlternateScreen)?;
        tty.execute(PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        ))?;
        tty.execute(SetCursorStyle::SteadyBar)?;
        Ok((Self { tty }, terminal_file))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.tty.execute(PopKeyboardEnhancementFlags);
        let _ = self.tty.execute(SetCursorStyle::DefaultUserShape);
        let _ = self.tty.execute(Show);
        let _ = self.tty.execute(LeaveAlternateScreen);
        let _ = self.tty.flush();
        let _ = disable_raw_mode();
    }
}

struct ChatRunOutcome {
    result: anyhow::Result<()>,
    state: ChatState,
}

impl ChatRunOutcome {
    fn ok(state: ChatState) -> Self {
        Self {
            result: Ok(()),
            state,
        }
    }

    fn err(state: ChatState, error: impl Into<anyhow::Error>) -> Self {
        Self {
            result: Err(error.into()),
            state,
        }
    }
}

#[derive(Clone)]
struct CapturedOutput {
    stream: CapturedStream,
    content: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapturedStream {
    Stdout,
    Stderr,
}

struct OutputCapture {
    rx: Receiver<CapturedOutput>,
    fd_capture: Option<FdCapture>,
    readers: Vec<JoinHandle<()>>,
}

impl OutputCapture {
    fn start() -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (fd_capture, readers) = FdCapture::start(tx)?;
        Ok(Self {
            rx,
            fd_capture: Some(fd_capture),
            readers,
        })
    }

    fn drain_to_state(&mut self, state: &mut ChatState) -> bool {
        let mut drained = false;
        while let Ok(output) = self.rx.try_recv() {
            state.push_output(&output);
            drained = true;
        }
        drained
    }

    fn finish(mut self) -> Vec<CapturedOutput> {
        self.restore();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        let mut remaining = Vec::new();
        while let Ok(output) = self.rx.try_recv() {
            remaining.push(output);
        }
        remaining
    }

    fn restore(&mut self) {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
        if let Some(mut fd_capture) = self.fd_capture.take() {
            fd_capture.restore();
        }
    }
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        self.restore();
    }
}

struct FdCapture {
    stdout_fd: Option<RawFd>,
    stderr_fd: Option<RawFd>,
}

impl FdCapture {
    fn start(tx: Sender<CapturedOutput>) -> io::Result<(Self, Vec<JoinHandle<()>>)> {
        let stdout_fd = dup_fd(libc::STDOUT_FILENO)?;
        let stderr_fd = dup_fd(libc::STDERR_FILENO)?;
        let stdout_pipe = make_pipe()?;
        let stderr_pipe = make_pipe()?;

        dup2_fd(stdout_pipe[1], libc::STDOUT_FILENO)?;
        dup2_fd(stderr_pipe[1], libc::STDERR_FILENO)?;
        close_fd(stdout_pipe[1]);
        close_fd(stderr_pipe[1]);

        let readers = vec![
            spawn_reader(stdout_pipe[0], CapturedStream::Stdout, tx.clone()),
            spawn_reader(stderr_pipe[0], CapturedStream::Stderr, tx),
        ];

        Ok((
            Self {
                stdout_fd: Some(stdout_fd),
                stderr_fd: Some(stderr_fd),
            },
            readers,
        ))
    }

    fn restore(&mut self) {
        if let Some(fd) = self.stdout_fd.take() {
            let _ = dup2_fd(fd, libc::STDOUT_FILENO);
            close_fd(fd);
        }
        if let Some(fd) = self.stderr_fd.take() {
            let _ = dup2_fd(fd, libc::STDERR_FILENO);
            close_fd(fd);
        }
    }
}

impl Drop for FdCapture {
    fn drop(&mut self) {
        self.restore();
    }
}

fn spawn_reader(fd: RawFd, stream: CapturedStream, tx: Sender<CapturedOutput>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut file = unsafe { File::from_raw_fd(fd) };
        let mut buffer = [0_u8; 4096];

        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let content = String::from_utf8_lossy(&buffer[..n]).to_string();
                    if tx.send(CapturedOutput { stream, content }).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn make_pipe() -> io::Result<[RawFd; 2]> {
    let mut fds = [0; 2];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fds)
    }
}

fn dup_fd(fd: RawFd) -> io::Result<RawFd> {
    let new_fd = unsafe { libc::dup(fd) };
    if new_fd == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(new_fd)
    }
}

fn dup2_fd(from: RawFd, to: RawFd) -> io::Result<()> {
    let rc = unsafe { libc::dup2(from, to) };
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn close_fd(fd: RawFd) {
    let _ = unsafe { libc::close(fd) };
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

    fn push_output(&mut self, output: &CapturedOutput) {
        if output.content.is_empty() {
            return;
        }

        let role = match output.stream {
            CapturedStream::Stdout => MessageRole::Stdout,
            CapturedStream::Stderr => MessageRole::Stderr,
        };
        let title = match output.stream {
            CapturedStream::Stdout => "LOG",
            CapturedStream::Stderr => "ERR",
        };

        if let Some(last) = self.messages.last_mut() {
            if last.role == role {
                last.content.push_str(&output.content);
                self.follow_tail = true;
                return;
            }
        }

        self.messages.push(ChatMessage {
            role,
            title: title.to_string(),
            agent_id: String::new(),
            content: output.content.clone(),
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
    fn label(&self, fallback_agent: &str) -> String {
        match self.role {
            MessageRole::User => "You".to_string(),
            MessageRole::Assistant | MessageRole::Thought | MessageRole::Tool => {
                self.agent_header_label(fallback_agent)
            }
            MessageRole::Stdout => "STDOUT".to_string(),
            MessageRole::Stderr => "STDERR".to_string(),
            MessageRole::System => "SYSTEM".to_string(),
            MessageRole::Error => "ERROR".to_string(),
        }
    }

    fn agent_header_label(&self, fallback_agent: &str) -> String {
        let agent_id = if self.agent_id.is_empty() {
            fallback_agent
        } else {
            &self.agent_id
        };

        if self.title.is_empty() {
            agent_id.to_string()
        } else {
            format!("[{}] {}", agent_id, self.title)
        }
    }

    fn render_lines(&self, fallback_agent: &str) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let label = self.label(fallback_agent);
        let color = self.role.label_color();

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
    Stdout,
    Stderr,
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
            Self::User | Self::Stdout | Self::Stderr | Self::System | Self::Error => false,
        }
    }

    fn body_style(self) -> Style {
        match self {
            Self::Thought => Style::default().fg(Color::DarkGray),
            Self::Tool => Style::default().fg(Color::Yellow),
            Self::Stdout => Style::default().fg(Color::Magenta),
            Self::Stderr => Style::default().fg(Color::Red),
            Self::Error => Style::default().fg(Color::Red),
            _ => Style::default().fg(Color::White),
        }
    }

    fn label_color(self) -> Color {
        match self {
            Self::User => Color::Green,
            Self::Assistant => Color::Green,
            Self::Thought => Color::Green,
            Self::Tool => Color::Green,
            Self::Stdout => Color::Magenta,
            Self::Stderr => Color::Red,
            Self::System => Color::Blue,
            Self::Error => Color::Red,
        }
    }
}

fn wrapped_height(lines: &[Line<'_>], width: u16) -> usize {
    Paragraph::new(Text::from(lines.to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(width)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_height_uses_paragraph_word_wrapping() {
        let lines = vec![Line::raw("aaaaa aaaaa aaaaa")];

        assert_eq!(wrapped_height(&lines, 10), 3);
    }

    #[test]
    fn input_cursor_position_handles_wrapping_and_newlines() {
        let area = Rect::new(10, 20, 4, 3);

        assert_eq!(input_cursor_position("abc", 0, area), (10, 20));
        assert_eq!(input_cursor_position("abc", 2, area), (12, 20));
        assert_eq!(input_cursor_position("abcd", 4, area), (13, 20));
        assert_eq!(input_cursor_position("abcde", 5, area), (11, 21));
        assert_eq!(input_cursor_position("a\nc", 2, area), (10, 21));
    }
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

fn replay_scrollback(state: &ChatState, fallback_agent: &str) {
    let mut stdout = io::stdout();

    for message in &state.messages {
        let _ = writeln!(stdout, "› {}", message.label(fallback_agent));
        for line in message.content.split('\n') {
            let _ = writeln!(stdout, "  {line}");
        }
        let _ = writeln!(stdout);
    }
    let _ = stdout.flush();
}
