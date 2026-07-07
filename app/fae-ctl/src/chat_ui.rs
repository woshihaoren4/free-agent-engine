use crossterm::{
    cursor::{MoveToColumn, SetCursorStyle},
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyModifiers},
    queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode},
};
use fae_agent::{GLOBAL_KEY_PROJECT_DIR, MemoryEntry, ModelCallConfig, Record, SingleSessionMD};
use fae_engine::Workspace;
use std::collections::HashMap;
use std::io::{self, Write};
use std::pin::Pin;
use tokio_stream::StreamExt;
use tui_input::{Input, InputRequest, backend::crossterm::EventHandler};

pub struct ChatUi {
    ws: Workspace,
    agent_id: String,
    session_id: Option<String>,
    model_name: ModelCallConfig,
}

#[derive(Default)]
struct RecordOutputCache {
    current_titles: HashMap<String, String>,
    pending: Vec<PendingRecordOutput>,
}

struct PendingRecordOutput {
    agent_id: String,
    title: String,
    content: String,
}

impl RecordOutputCache {
    fn push<F>(
        &mut self,
        record: &Record,
        current_output_agent_id: &mut String,
        current_output_title: &mut String,
        print_text: &F,
    ) -> io::Result<()>
    where
        F: Fn(&str) -> io::Result<()>,
    {
        let agent_id = record.agent_id.clone();
        let title = record.title();

        if self
            .current_titles
            .get(&agent_id)
            .is_some_and(|current_title| current_title != &title)
        {
            self.flush_agent(
                &agent_id,
                current_output_agent_id,
                current_output_title,
                print_text,
            )?;
        }

        self.current_titles.insert(agent_id.clone(), title.clone());

        let content = record.content();
        if content.is_empty() {
            return Ok(());
        }

        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|pending| pending.agent_id == agent_id && pending.title == title)
        {
            pending.content.push_str(content);
        } else {
            self.pending.push(PendingRecordOutput {
                agent_id,
                title,
                content: content.to_string(),
            });
        }

        Ok(())
    }

    fn flush_agent<F>(
        &mut self,
        agent_id: &str,
        current_output_agent_id: &mut String,
        current_output_title: &mut String,
        print_text: &F,
    ) -> io::Result<()>
    where
        F: Fn(&str) -> io::Result<()>,
    {
        let mut index = 0;
        while index < self.pending.len() {
            if self.pending[index].agent_id == agent_id {
                let pending = self.pending.remove(index);
                ChatUi::print_record_output(
                    &pending.agent_id,
                    &pending.title,
                    &pending.content,
                    current_output_agent_id,
                    current_output_title,
                    print_text,
                )?;
            } else {
                index += 1;
            }
        }
        self.current_titles.remove(agent_id);
        Ok(())
    }

    fn flush_all<F>(
        &mut self,
        current_output_agent_id: &mut String,
        current_output_title: &mut String,
        print_text: &F,
    ) -> io::Result<()>
    where
        F: Fn(&str) -> io::Result<()>,
    {
        for pending in self.pending.drain(..) {
            ChatUi::print_record_output(
                &pending.agent_id,
                &pending.title,
                &pending.content,
                current_output_agent_id,
                current_output_title,
                print_text,
            )?;
        }
        self.current_titles.clear();
        Ok(())
    }

    fn clear(&mut self) {
        self.current_titles.clear();
        self.pending.clear();
    }
}

struct TerminalSession;

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;

        let session = Self;
        if let Err(err) = Self::configure_terminal(SetCursorStyle::SteadyBar, true) {
            drop(session);
            return Err(err);
        }

        Ok(session)
    }

    fn configure_terminal(style: SetCursorStyle, bracketed_paste: bool) -> io::Result<()> {
        let mut stdout = io::stdout();
        if bracketed_paste {
            queue!(stdout, style, EnableBracketedPaste)?;
        } else {
            queue!(stdout, style, DisableBracketedPaste)?;
        }
        stdout.flush()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = Self::configure_terminal(SetCursorStyle::DefaultUserShape, false);
        let _ = disable_raw_mode();
    }
}

impl ChatUi {
    pub fn new(ws: Workspace, agent_id: String, session_id: Option<String>) -> Self {
        let model_name = ModelCallConfig::default();
        Self {
            ws,
            agent_id,
            session_id,
            model_name,
        }
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        let terminal_session = TerminalSession::enter()?;

        let agent = self.ws.get_agent(&self.agent_id).await?.on_info().await;

        self.model_name = agent.model();

        let res = self.run_app().await;

        drop(terminal_session);
        println!("\r");

        res?;
        Ok(())
    }

    async fn run_app(&mut self) -> io::Result<()> {
        let mut input = Input::default();

        let clear_line = || -> io::Result<()> {
            let mut stdout = io::stdout();
            queue!(stdout, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
            stdout.flush()
        };

        let print_text = |text: &str| -> io::Result<()> {
            let mut stdout = io::stdout();
            queue!(stdout, Clear(ClearType::FromCursorDown))?;
            let text = text.replace('\n', "\r\n");
            print!("{}", text);
            queue!(
                stdout,
                crossterm::style::Print("\n"),
                crossterm::cursor::MoveUp(1)
            )?;
            stdout.flush()
        };

        let mut session_config = SingleSessionMD::default().set(GLOBAL_KEY_PROJECT_DIR, ".");
        if let Some(session_id) = self.session_id.as_ref() {
            session_config = session_config.set_id(session_id);
        }
        let mut session_id = session_config.id.clone();
        let mut user_id = session_config.user_id.clone();
        let mut session = match self
            .ws
            .session_call_stream::<_, Record, Record>(&self.agent_id, session_config)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                clear_line()?;
                print_text(&format!("Failed to create session: {:?}\n", e))?;
                return Ok(());
            }
        };

        let mut stream_active = false;
        let mut current_stream: Option<Pin<Box<dyn tokio_stream::Stream<Item = Record> + Send>>> =
            None;
        let mut pending_outputs = RecordOutputCache::default();
        let mut current_output_agent_id = String::new();
        let mut current_output_title = String::new();
        let mut current_title = String::new();
        let mut spinner_tick: usize = 0;
        let mut user_input = String::new();

        // Print welcome header
        clear_line()?;
        self.print_welcome_banner()?;
        print_text(
            "Instructions for Use:\n - '/exit' to quit.\n - '/reset' to restart session.\n - 'ctrl+j' line break.\n - 'ctrl+c' to abort session or quit cli.\n\n",
        )?;

        loop {
            Self::redraw_prompt(&input, stream_active, spinner_tick, &current_title)?;

            let mut event_handled = false;
            let poll_timeout = if stream_active { 50 } else { 100 };

            if crossterm::event::poll(std::time::Duration::from_millis(poll_timeout))? {
                let event = event::read()?;
                event_handled = true;
                match event {
                    Event::Key(key) => match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if stream_active {
                                stream_active = false;
                                current_stream = None;
                                pending_outputs.clear();
                                current_output_agent_id.clear();
                                current_output_title.clear();
                                clear_line()?;
                                print_text("\n[Session aborted. Starting a new session...]\n\n")?;
                                let session_config = SingleSessionMD::default();
                                session_id = session_config.id.clone();
                                user_id = session_config.user_id.clone();
                                session = match self
                                    .ws
                                    .session_call_stream::<_, Record, Record>(
                                        &self.agent_id,
                                        session_config,
                                    )
                                    .await
                                {
                                    Ok(s) => s,
                                    Err(e) => {
                                        clear_line()?;
                                        print_text(&format!(
                                            "Failed to create new session: {:?}\n",
                                            e
                                        ))?;
                                        return Ok(());
                                    }
                                };
                                input.reset();
                            } else {
                                return Ok(());
                            }
                        }
                        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            //换行
                            let val = input.value().to_string();
                            let val = val.trim();
                            let is_header = user_input.is_empty();
                            Self::print_user_message(val, is_header)?;
                            Self::append_user_input(&mut user_input, val);
                            input.reset();
                            clear_line()?;
                        }
                        KeyCode::Enter => {
                            let val = input.value().to_string();
                            let val = val.trim();
                            if val.starts_with("/exit") {
                                return Ok(());
                            } else if val.starts_with("/reset") {
                                if stream_active {
                                    stream_active = false;
                                    current_stream = None;
                                    pending_outputs.clear();
                                    current_output_agent_id.clear();
                                    current_output_title.clear();
                                }
                                clear_line()?;
                                if let Err(e) = self
                                    .ws
                                    .session_reset(&self.agent_id, &user_id, &session_id)
                                    .await
                                {
                                    print_text(&format!("Failed to reset session: {:?}\n", e))?;
                                } else {
                                    let text = " Session reset successfully ";
                                    let cols =
                                        crossterm::terminal::size().map(|(c, _)| c).unwrap_or(80)
                                            as usize;
                                    let inner_cols = cols.saturating_sub(4);
                                    let dashes_len = inner_cols.saturating_sub(text.len()) / 2;
                                    let dashes = "-".repeat(dashes_len);
                                    print_text(&format!("\n{}{}{}\n\n", dashes, text, dashes))?;
                                }
                                input.reset();
                            } else if !val.is_empty() || !user_input.is_empty() {
                                if !val.is_empty() {
                                    let is_header = user_input.is_empty();
                                    Self::print_user_message(val, is_header)?;
                                    Self::append_user_input(&mut user_input, val);
                                    input.reset();
                                    clear_line()?;
                                }
                                if !stream_active {
                                    let msg = Record::from_user_input(user_input.as_str());
                                    match session.call_stream(msg).await {
                                        Ok(s) => {
                                            pending_outputs.clear();
                                            current_output_agent_id.clear();
                                            current_output_title.clear();
                                            current_stream = Some(Pin::from(s));
                                            stream_active = true;
                                            current_title = "Waiting".to_string();
                                        }
                                        Err(e) => {
                                            print_text(&format!("Failed to send chat: {:?}\n", e))?;
                                        }
                                    }
                                    user_input.clear();
                                }
                            }
                        }
                        _ => {
                            input.handle_event(&Event::Key(key));
                        }
                    },
                    Event::Paste(pasted) => {
                        Self::insert_paste(&mut input, &mut user_input, &pasted)?;
                        clear_line()?;
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
                                    if record.agent_id == self.agent_id {
                                        current_title = record.title();
                                        pending_outputs.flush_all(
                                            &mut current_output_agent_id,
                                            &mut current_output_title,
                                            &print_text,
                                        )?;
                                        Self::print_record(
                                            &record,
                                            &mut current_output_agent_id,
                                            &mut current_output_title,
                                            &print_text,
                                        )?;
                                    } else {
                                        pending_outputs.push(
                                            &record,
                                            &mut current_output_agent_id,
                                            &mut current_output_title,
                                            &print_text,
                                        )?;
                                    }
                                }
                                None => {
                                    pending_outputs.flush_all(
                                        &mut current_output_agent_id,
                                        &mut current_output_title,
                                        &print_text,
                                    )?;
                                    stream_active = false;
                                    current_stream = None;
                                    print_text("\n\n")?;
                                }
                            }
                            spinner_tick = spinner_tick.wrapping_add(1);
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(select_timeout)) => {
                            spinner_tick = spinner_tick.wrapping_add(1);
                        }
                    }
                } else {
                    stream_active = false;
                }
            }
        }
    }

    fn append_user_input(user_input: &mut String, val: &str) {
        if !user_input.is_empty() {
            user_input.push('\n');
        }
        user_input.push_str(val);
    }

    fn insert_paste(input: &mut Input, user_input: &mut String, pasted: &str) -> io::Result<()> {
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let mut lines = pasted.split('\n').peekable();

        while let Some(line) = lines.next() {
            for c in line.chars() {
                input.handle(InputRequest::InsertChar(c));
            }

            if lines.peek().is_some() {
                let val = input.value().to_string();
                let is_header = user_input.is_empty();
                Self::print_user_message(&val, is_header)?;
                Self::append_user_input(user_input, &val);
                input.reset();
            }
        }

        Ok(())
    }

    fn print_welcome_banner(&self) -> io::Result<()> {
        let mut stdout = io::stdout();

        let version = env!("CARGO_PKG_VERSION");
        let directory = std::env::current_dir()
            .ok()
            .and_then(|p| {
                let s = p.display().to_string();
                let home = std::env::var("HOME").ok();
                Some(match home {
                    Some(h) if s.starts_with(&h) => format!("~{}", &s[h.len()..]),
                    _ => s,
                })
            })
            .unwrap_or_else(|| ".".to_string());

        // Banner content lines (without borders).
        let title = format!(">_ Free Agent Engine CLI (v{})", version);
        let model_line = format!(
            "agent: {}    model: {}",
            &self.agent_id, &self.model_name.model
        );
        let dir_line = format!("directory: {}", directory);

        let lines = [title.as_str(), "", model_line.as_str(), dir_line.as_str()];

        // Inner width = longest line, with 1 space padding on each side.
        let content_width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let inner_width = content_width + 2;

        let top = format!("╭{}╮", "─".repeat(inner_width));
        let bottom = format!("╰{}╯", "─".repeat(inner_width));

        queue!(
            stdout,
            MoveToColumn(0),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("{}\r\n", top)),
        )?;

        for line in lines {
            let pad = content_width - line.chars().count();
            let body = format!(" {}{} ", line, " ".repeat(pad));
            queue!(
                stdout,
                MoveToColumn(0),
                SetForegroundColor(Color::DarkGrey),
                Print("│"),
                SetForegroundColor(Color::White),
                Print(&body),
                SetForegroundColor(Color::DarkGrey),
                Print("│\r\n"),
            )?;
        }

        queue!(
            stdout,
            MoveToColumn(0),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("{}\r\n", bottom)),
            ResetColor,
            Print("\r\n"),
        )?;
        stdout.flush()
    }

    fn print_record<F>(
        record: &Record,
        current_output_agent_id: &mut String,
        current_output_title: &mut String,
        print_text: &F,
    ) -> io::Result<()>
    where
        F: Fn(&str) -> io::Result<()>,
    {
        let title = record.title();
        Self::print_record_output(
            &record.agent_id,
            &title,
            record.content(),
            current_output_agent_id,
            current_output_title,
            print_text,
        )
    }

    fn print_record_output<F>(
        agent_id: &str,
        title: &str,
        content: &str,
        current_output_agent_id: &mut String,
        current_output_title: &mut String,
        print_text: &F,
    ) -> io::Result<()>
    where
        F: Fn(&str) -> io::Result<()>,
    {
        if current_output_agent_id != agent_id || current_output_title != title {
            current_output_agent_id.clear();
            current_output_agent_id.push_str(agent_id);
            current_output_title.clear();
            current_output_title.push_str(title);

            let title_suffix =
                if current_output_title.is_empty() || current_output_title == "Waiting" {
                    String::new()
                } else {
                    format!(" [{}]", current_output_title)
                };
            Self::print_agent_header(agent_id, &title_suffix, print_text)?;
        }

        if content.is_empty() {
            return Ok(());
        }

        if current_output_title == "Thinking" {
            let mut stdout = io::stdout();
            queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
            stdout.flush()?;
            print_text(content)?;
            queue!(stdout, ResetColor)?;
            stdout.flush()?;
        } else if current_output_title.starts_with("CallTool")
            || current_output_title.starts_with("ToolOut")
        {
            let mut stdout = io::stdout();
            queue!(stdout, SetForegroundColor(Color::Yellow))?;
            stdout.flush()?;
            print_text(content)?;
            queue!(stdout, ResetColor)?;
            stdout.flush()?;
        } else {
            print_text(content)?;
        }

        Ok(())
    }

    fn print_agent_header<F>(agent_id: &str, title_suffix: &str, print_text: &F) -> io::Result<()>
    where
        F: Fn(&str) -> io::Result<()>,
    {
        let mut stdout = io::stdout();
        queue!(stdout, SetForegroundColor(Color::Green))?;
        stdout.flush()?;
        print_text(&format!("\n\n❯ {}{}\n", agent_id, title_suffix))?;
        queue!(stdout, ResetColor)?;
        stdout.flush()
    }

    fn redraw_prompt(
        input: &Input,
        stream_active: bool,
        spinner_tick: usize,
        title: &str,
    ) -> io::Result<()> {
        let mut stdout = io::stdout();

        if stream_active {
            let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let spinner = spinners[(spinner_tick / 2) % spinners.len()];
            let msg = format!("{} {}...", spinner, title);
            queue!(
                stdout,
                crossterm::cursor::SavePosition,
                crossterm::cursor::MoveToNextLine(1),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine),
                crossterm::style::SetForegroundColor(crossterm::style::Color::Yellow),
                crossterm::style::Print(&msg),
                crossterm::style::ResetColor,
                crossterm::cursor::RestorePosition
            )?;
        } else {
            queue!(stdout, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
            queue!(
                stdout,
                SetForegroundColor(Color::Blue),
                SetAttribute(Attribute::Bold),
                Print("❯ "),
                ResetColor,
                SetAttribute(Attribute::Reset),
                Print(input.value())
            )?;

            let cursor_pos = input.visual_cursor() as u16 + 2;
            queue!(stdout, MoveToColumn(cursor_pos))?;
        }
        stdout.flush()
    }

    fn print_user_message(val: &str, is_header: bool) -> io::Result<()> {
        let mut stdout = io::stdout();
        if is_header {
            let cols = crossterm::terminal::size().map(|(c, _)| c).unwrap_or(80) as usize;
            let separator = "─".repeat(cols);
            queue!(
                stdout,
                MoveToColumn(0),
                SetForegroundColor(Color::DarkGrey),
                Print(format!("{}\r\n", separator)),
                SetForegroundColor(Color::White),
                SetAttribute(Attribute::Bold),
                Print("❯ You\r\n"),
                SetAttribute(Attribute::Reset),
                SetForegroundColor(Color::White),
                Print(val.replace('\n', "\r\n")),
                ResetColor,
                Print("\r\n"),
            )?;
        } else {
            queue!(
                stdout,
                MoveToColumn(0),
                Clear(ClearType::CurrentLine),
                SetForegroundColor(Color::White),
                Print(val.replace('\n', "\r\n")),
                ResetColor,
                Print("\r\n"),
            )?;
        }
        stdout.flush()
    }
}
