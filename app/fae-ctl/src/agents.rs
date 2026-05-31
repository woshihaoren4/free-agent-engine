use fae_engine::{AgentsEngine, SingleAgentCtlFromFile, Workspace};
use crate::args::AgentArgs;
use crate::init_project::InitProject;

pub struct Agents{
    engine:AgentsEngine,
    ws:Workspace,
}

impl Agents{
    pub async fn new(ws:&str)->Self{
        let mut engine = AgentsEngine::default().await;
        let ws_dir = InitProject::get_workspace_dir(ws);
        let ws = engine.build_workspace(ws,|builder|{
            builder.set_loader(SingleAgentCtlFromFile::new(ws_dir));
        }).await;
        Self{
            engine,
            ws,
        }
    }
    pub async fn exit(&self){
        self.engine.exit().await;
    }
    pub async fn chat(&self, agent_name: &str, initial_chat: Option<String>) {
        let mut ui = crate::chat_ui::ChatUi::new(self.ws.clone(), agent_name.to_string(), initial_chat);
        if let Err(e) = ui.run().await {
            eprintln!("UI error: {:?}", e);
        }
    }
    pub async fn agents_list(&self){
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
    pub async fn exec(wd:String,args:AgentArgs){
        let this = Self::new(&wd).await;
        let agent = args.name.unwrap_or("main".to_string());
        if let Some(chat) = args.chat {
            this.chat(&agent, Some(chat)).await;
        } else {
            this.agents_list().await;
        }
        this.exit().await;
    }
}
