use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use wd_tools::PFErr;
use crate::{utils, AgentConfig, ModelCallConfig, ToolConfig, FAE_DEFAULT_MODEL, OPENAI_DEFAULT_MODEL};
use super::{EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL,
};

/// AgentConfig 的序列化数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigData {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub model: ModelCallConfig,
    #[serde(default = "default_prompt_dir")]
    pub prompt_dir: String,
    #[serde(default)]
    pub tools: Vec<ToolConfig>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub sub_agents: Vec<String>,
    #[serde(default)]
    pub custom: HashMap<String, String>,
}

fn default_channel() -> String {
    EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL.to_string()
}

fn default_prompt_dir() -> String {
    "system.txt".to_string()
}

impl Default for AgentConfigData {
    fn default() -> Self {
        // 模型名称从环境变量中加载，如果未设置则使用默认值
        let model_name = std::env::var(FAE_DEFAULT_MODEL)
            .or_else(|_| std::env::var(OPENAI_DEFAULT_MODEL))
            .unwrap_or_else(|_| "gpt-4o".to_string());

        Self {
            name: "风筝引擎".to_string(),
            description: String::new(),
            model: ModelCallConfig {
                model: model_name,
                channel: default_channel(),
                max_chat_history_round: 20,
                //    1:Minimal,2:Low, 3:Medium, 4:High,
                reasoning_effort: Some(2),
                frequency_penalty: None,
                max_tokens: None,
                max_completion_tokens: None,
                presence_penalty: None,
                temperature: Some(1.0),
                top_p: Some(1.0),
            },
            prompt_dir: default_prompt_dir(),
            tools: vec![
                ToolConfig::new("execute_command"),
                ToolConfig::new("read_file"),
                ToolConfig::new("write_file"),
                ToolConfig::new("send_http_request"),
                ToolConfig::new("execute_python"),
                ToolConfig::new("todo_write"),
            ],
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            sub_agents: Vec::new(),
            custom: HashMap::new(),
        }
    }
}

impl AgentConfigData {
    pub async fn init<P: Into<PathBuf>>(&mut self,agent_id: &str, file_path: P)->anyhow::Result<()> {
        let path = file_path.into();
        // 自检配置
        let prompt_dir = if self.prompt_dir.starts_with("/") {
            self.prompt_dir.clone()
        }else{
            let dir = format!("{}/{}/{}", path.display(),agent_id, self.prompt_dir);
            // self.prompt_dir = dir.clone();
            dir
        };
        //
        //创建prompt文件
        let prompt_path = PathBuf::from(prompt_dir);
        if !prompt_path.exists() {
            return anyhow::anyhow!("[AgentConfigData] Prompt file not found: {}", prompt_path.display()).err();
        }
        //创建配置文件
        let config_path = path.join(agent_id).join("config.json");
        if !config_path.exists() {
            let content = serde_json::to_string_pretty(self)?;
            tokio::fs::write(&config_path, content)
                .await
                .context("Failed to write agent config")?;
        }
        Ok(())
    }
    pub fn into_agent_config(self) -> AgentConfigFile {
        AgentConfigFile {
            prompt: "".to_string(),
            prompt_path: "".to_string(),
            config_path: "".to_string(),
            config: self,
        }
    }
    pub fn set_prompt_path<P: Into<String>>(mut self, prompt_path: P)->Self {
        self.prompt_dir = prompt_path.into();self
    }
    pub fn set_name(mut self, name: &str) ->Self {
        self.name = name.to_string();
        self
    }
    pub fn set_description(mut self, description: &str)->Self {
        self.description = description.to_string();
        self
    }
}

/// 基于文件系统的 AgentConfig 实现
pub struct AgentConfigFile {
    prompt: String,
    prompt_path: String,
    config_path: String,
    config: AgentConfigData,
}

impl AgentConfigFile {

    /// 从指定文件路径加载配置，如果文件不存在则创建默认配置
    pub async fn load<P: Into<PathBuf>>(agent_dir: P) -> anyhow::Result<Self> {
        let agent_dir = agent_dir.into();
        let config_path = agent_dir.join("config.json");

        let config_data = if config_path.exists() {
            let content = tokio::fs::read_to_string(&config_path)
                .await
                .context("Failed to read agent config file")?;
            serde_json::from_str::<AgentConfigData>(&content).unwrap_or_default()
        } else {
            return Err(anyhow::anyhow!("[AgentConfigFile] Agent config file not found: {}", config_path.display()));
        };

        let prompt_path = if config_data.prompt_dir.starts_with("/") {
            PathBuf::from(config_data.prompt_dir.clone())
        }else{
            let dir = format!("{}/{}", agent_dir.display(), config_data.prompt_dir);
            PathBuf::from(dir)
        };

        let prompt = if prompt_path.exists() {
            let content = tokio::fs::read_to_string(&prompt_path)
                .await
                .context("Failed to read prompt file")?;
            content
        } else {
            return Err(anyhow::anyhow!("[AgentConfigFile] Prompt file not found: {}", prompt_path.display()));
        };

        Ok(Self {
            prompt,
            prompt_path: utils::path_clean(prompt_path).display().to_string(),
            config_path: utils::path_clean(config_path).display().to_string(),
            config: config_data,
        })
    }

    /// 内部方法：保存配置到文件
    async fn save(&self, data: &AgentConfigData) -> anyhow::Result<()> {
        let content = serde_json::to_string_pretty(data)?;
        tokio::fs::write(&self.config_path, content)
            .await
            .context("Failed to write agent config")?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl AgentConfig for AgentConfigFile {
    fn name(&self) -> String {
        self.config.name.clone()
    }

    fn desc(&self) -> String {
        self.config.description.clone()
    }

    fn model(&self) -> ModelCallConfig {
        self.config.model.clone()
    }

    fn prompt(&self) -> String {
        self.prompt.clone()
    }

    fn tools(&self) -> Vec<ToolConfig> {
        self.config.tools.clone()
    }

    fn skills(&self) -> Vec<String> {
        self.config.skills.clone()
    }

    fn mcp_servers(&self) -> Vec<String> {
        self.config.mcp_servers.clone()
    }

    fn sub_agents(&self) -> Vec<String> {
        self.config.sub_agents.clone()
    }

    fn get(&self, key: &str) -> Option<String> {
        self.config.custom.get(key).cloned()
    }
    fn metadata(&self,id:&str) -> String {
        let mut meta = "\n## Your Agent Metadata:".to_string();
        meta.push_str(&format!("\n - your Agent Name: `{}`", self.name()));
        meta.push_str(&format!("\n - your Agent Id: `{}`", id));
        meta.push_str(&format!("\n - your model,tools,skill,mcp_servers,sub_agents config file path: {}", self.config_path));
        format!("{}\n - your enactment prompt file path: {}", meta, self.prompt_path)
    }

    async fn init(&mut self, id:&str, workspace:&str, _cfg:serde_json::Value) -> anyhow::Result<()> {
        self.config.init(id,PathBuf::from(workspace)).await?;
        Ok(())
    }
}
