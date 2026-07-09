use crate::args::{AgentArgs, DEFAULT_AGENT_ID, DEFAULT_USER_ID};
use fae_agent::{GLOBAL_KEY_PROJECT_DIR, MemoryEntry, Record, SingleSessionMD};
use fae_engine::{AgentsEngine, Workspace};
use std::io::{self, Write};
use std::pin::Pin;
use tokio_stream::StreamExt;

pub struct Agents {
    engine: AgentsEngine,
    ws: Workspace,
}

impl Agents {
    pub async fn new(ws: &str) -> Self {
        let engine = AgentsEngine::default().await;
        let ws = engine.workspace(ws).expect("No workspace found");
        Self { engine, ws }
    }
    pub async fn exit(&self) {
        self.engine.exit().await;
    }
    pub async fn chat(&self, agent_name: &str, args: AgentArgs) {
        let session_id = if args.new_session {
            Some(wd_tools::uuid::v4())
        } else {
            args.session_id
        };
        let mut ui = crate::chat_ui::ChatUi::new(self.ws.clone(), agent_name.to_string(), session_id);
        if let Err(e) = ui.run().await {
            eprintln!("UI error: {:?}", e);
        }
    }
    pub async fn stdio_chat(&self, agent_name: &str, args: AgentArgs) -> anyhow::Result<()> {
        let mut session_config = SingleSessionMD::default()
            .set_user_id(args.user.unwrap_or(DEFAULT_USER_ID.to_string()))
            .set(GLOBAL_KEY_PROJECT_DIR, ".");

        if args.new_session {
            session_config = session_config.set_id(wd_tools::uuid::v4());
        } else if let Some(session_id) = args.session_id {
            session_config = session_config.set_id(session_id);
        }

        let mut session = self
            .ws
            .session_call_stream::<_, Record, Record>(agent_name, session_config)
            .await?;
        let stdin = io::stdin();
        let mut stdout = io::stdout();

        loop {
            let mut input = String::new();
            let bytes = stdin.read_line(&mut input)?;
            if bytes == 0 {
                break;
            }

            let input = input.trim();
            if input.is_empty() {
                continue;
            }
            if input == "/exit" {
                break;
            }
            let mut title = String::new();
            let mut stream = Pin::from(session.call_stream(Record::from_user_input(input)).await?);
            while let Some(record) = stream.next().await {
                let t = record.title();
                if !t.is_empty() && t != "Waiting" && title != t {
                    title = t;
                    println!("\n---> {} <---", title);
                    stdout.flush()?;
                }
                let content = record.content();
                if !content.is_empty() {
                    print!("{}", content);
                    stdout.flush()?;
                }
            }
            println!();
            stdout.flush()?;
        }
        Ok(())
    }
    pub async fn agents_list(&self) {
        match self.ws.list_agents(100, 0).await {
            Ok(agents) => {
                println!("Agents:");
                for agent in agents {
                    println!("  - {}: {}", agent.id(), agent.desc());
                }
            }
            Err(e) => {
                eprintln!("Failed to list agents: {:?}", e);
            }
        }
    }
    pub async fn chat_history(&self, agent_id: &str, user_id: &str) {
        let history = self
            .ws
            .session_history::<SingleSessionMD>(agent_id, user_id, 100)
            .await;
        let list = match history {
            Ok(history) => history,
            Err(e) => {
                eprintln!("Failed to get chat history: {:?}", e);
                return;
            }
        };
        println!("Chat history:");
        for session in list {
            println!("  - {}: {}", session.get_id(), session.get_name());
        }
    }
    pub async fn session_history(&self, agent_id: &str, user_id: &str) {
        let history = self
            .ws
            .session_history::<SingleSessionMD>(agent_id, user_id, 100)
            .await;
        let list = match history {
            Ok(history) => history,
            Err(e) => {
                eprintln!("Failed to get session history: {:?}", e);
                return;
            }
        };
        println!("Session history:");
        for session in list {
            println!("  - {}: {}", session.get_id(), session.get_name());
        }
    }
    pub async fn exec(wd: String, args: AgentArgs) {
        let this = Self::new(&wd).await;
        let agent = args.id.clone().unwrap_or(DEFAULT_AGENT_ID.to_string());
        let user_id = args.user.clone().unwrap_or(DEFAULT_USER_ID.to_string());
        if args.session_history {
            this.session_history(&agent, &user_id).await;
        } else if args.history {
            this.chat_history(&agent, &user_id).await;
        } else if args.stdio {
            if let Err(e) = this.stdio_chat(&agent, args).await {
                eprintln!("Stdio chat error: {:?}", e);
            }
        } else if args.chat {
            this.chat(&agent, args).await;
        } else {    
            this.agents_list().await;
        }
        this.exit().await;
    }
}
