use crate::args::AgentArgs;
use crate::init_project::InitProject;
use fae_agent::SingleSessionMD;
use fae_engine::{AgentsEngine, SingleAgentCtlFromFile, Workspace};

pub struct Agents {
    engine: AgentsEngine,
    ws: Workspace,
}

impl Agents {
    pub async fn new(ws: &str) -> Self {
        let mut engine = AgentsEngine::default().await;
        let ws_dir = InitProject::get_workspace_dir(ws);
        let ws = engine
            .build_workspace(ws, |builder| {
                builder.set_loader(SingleAgentCtlFromFile::new(ws_dir));
            })
            .await;
        Self { engine, ws }
    }
    pub async fn exit(&self) {
        self.engine.exit().await;
    }
    pub async fn chat(&self, agent_name: &str) {
        let mut ui = crate::chat_ui::ChatUi::new(self.ws.clone(), agent_name.to_string());
        if let Err(e) = ui.run().await {
            eprintln!("UI error: {:?}", e);
        }
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
    pub async fn exec(wd: String, args: AgentArgs) {
        let this = Self::new(&wd).await;
        let agent = args.id.unwrap_or("main".to_string());
        let user_id = args.user.unwrap_or("master".to_string());
        if args.history {
            this.chat_history(&agent, &user_id).await;
        } else if args.chat {
            this.chat(&agent).await;
        } else {
            this.agents_list().await;
        }
        this.exit().await;
    }
}
