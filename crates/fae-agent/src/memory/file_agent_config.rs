use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;

use super::{
    AgentConfig, DEFAULT_SYSTEM_PROMPT, EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL, ModelCallConfig,
};

/// AgentConfig 的序列化数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigData {
    pub name: String,
    pub model: ModelCallConfig,
    #[serde(default = "default_prompt_dir")]
    pub prompt_dir: String,
    #[serde(default)]
    pub tools: Vec<String>,
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
    "./".to_string()
}

impl Default for AgentConfigData {
    fn default() -> Self {
        // 模型名称从环境变量中加载，如果未设置则使用默认值
        let model_name = std::env::var("OPENAI_DEFAULT_MODEL")
            .or_else(|_| std::env::var("FAE_DEFAULT_MODEL"))
            .unwrap_or_else(|_| "gpt-4o".to_string());

        Self {
            name: "default_agent".to_string(),
            model: ModelCallConfig {
                model: model_name,
                channel: Some(default_channel()),
                reasoning_effort: None,
                frequency_penalty: None,
                max_completion_tokens: None,
                presence_penalty: None,
                temperature: Some(1.0),
                top_p: Some(1.0),
            },
            prompt_dir: default_prompt_dir(),
            tools: Vec::new(),
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            sub_agents: Vec::new(),
            custom: HashMap::new(),
        }
    }
}

/// 基于文件系统的 AgentConfig 实现
pub struct AgentConfigFile {
    file_path: PathBuf,
    config: RwLock<AgentConfigData>,
}

impl AgentConfigFile {
    /// 从指定文件路径加载配置，如果文件不存在则创建默认配置
    pub async fn load_or_default<P: Into<PathBuf>>(file_path: P) -> anyhow::Result<Self> {
        let path = file_path.into();
        let config_data = if path.exists() {
            let content = tokio::fs::read_to_string(&path)
                .await
                .context("Failed to read agent config file")?;
            serde_json::from_str::<AgentConfigData>(&content).unwrap_or_default()
        } else {
            let mut default_data = AgentConfigData::default();
            if let Some(parent) = path.parent() {
                if !parent.exists() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .context("Failed to create agent config directory")?;
                }
                // 默认将 prompt_dir 设置为配置文件同目录
                default_data.prompt_dir = parent.to_string_lossy().to_string();
            }
            let content = serde_json::to_string_pretty(&default_data)?;
            tokio::fs::write(&path, content)
                .await
                .context("Failed to write default agent config")?;

            // 写入默认的 prompt.txt
            let prompt_path = PathBuf::from(&default_data.prompt_dir).join("prompt.txt");
            if !prompt_path.exists() {
                tokio::fs::write(&prompt_path, DEFAULT_SYSTEM_PROMPT)
                    .await
                    .context("Failed to write default prompt file")?;
            }

            default_data
        };

        Ok(Self {
            file_path: path,
            config: RwLock::new(config_data),
        })
    }

    /// 内部方法：保存配置到文件
    async fn save(&self, data: &AgentConfigData) -> anyhow::Result<()> {
        let content = serde_json::to_string_pretty(data)?;
        tokio::fs::write(&self.file_path, content)
            .await
            .context("Failed to write agent config")?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl AgentConfig for AgentConfigFile {
    async fn name(&self) -> anyhow::Result<String> {
        let config = self.config.read().await;
        Ok(config.name.clone())
    }

    async fn model(&self) -> anyhow::Result<ModelCallConfig> {
        let config = self.config.read().await;
        Ok(config.model.clone())
    }

    async fn prompt(&self) -> anyhow::Result<String> {
        let config = self.config.read().await;
        let prompt_path = PathBuf::from(&config.prompt_dir).join("prompt.txt");
        if prompt_path.exists() {
            let content = tokio::fs::read_to_string(&prompt_path)
                .await
                .context("Failed to read prompt file")?;
            Ok(content)
        } else {
            Ok(DEFAULT_SYSTEM_PROMPT.to_string())
        }
    }

    async fn tools(&self) -> anyhow::Result<Vec<String>> {
        let config = self.config.read().await;
        Ok(config.tools.clone())
    }

    async fn skills(&self) -> anyhow::Result<Vec<String>> {
        let config = self.config.read().await;
        Ok(config.skills.clone())
    }

    async fn mcp_servers(&self) -> anyhow::Result<Vec<String>> {
        let config = self.config.read().await;
        Ok(config.mcp_servers.clone())
    }

    async fn sub_agents(&self) -> anyhow::Result<Vec<String>> {
        let config = self.config.read().await;
        Ok(config.sub_agents.clone())
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        let config = self.config.read().await;
        Ok(config.custom.get(key).cloned())
    }

    async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let mut config = self.config.write().await;
        config.custom.insert(key.to_string(), value.to_string());
        self.save(&config).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn get_temp_file() -> PathBuf {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("agent_config_{}.json", time))
    }

    #[tokio::test]
    async fn test_agent_config_file_default() {
        let file_path = get_temp_file();

        // 确保使用环境变量
        unsafe {
            std::env::set_var("MODEL_NAME", "test-model-4o");
        }

        let config_file = AgentConfigFile::load_or_default(&file_path).await.unwrap();

        assert_eq!(config_file.name().await.unwrap(), "default_agent");
        assert_eq!(config_file.model().await.unwrap().model, "test-model-4o");
        assert_eq!(
            config_file
                .model()
                .await
                .unwrap()
                .channel
                .as_deref()
                .unwrap_or_default(),
            EXECUTOR_OPENAI_COMPATIBLE_API_CHANNEL
        );
        assert_eq!(config_file.prompt().await.unwrap(), DEFAULT_SYSTEM_PROMPT);

        // 自定义配置测试
        assert_eq!(config_file.get("my_key").await.unwrap(), None);
        config_file.set("my_key", "my_value").await.unwrap();
        assert_eq!(
            config_file.get("my_key").await.unwrap(),
            Some("my_value".to_string())
        );

        // 测试从已保存的文件加载
        let config_file2 = AgentConfigFile::load_or_default(&file_path).await.unwrap();
        assert_eq!(
            config_file2.get("my_key").await.unwrap(),
            Some("my_value".to_string())
        );

        tokio::fs::remove_file(file_path).await.ok();
        unsafe {
            std::env::remove_var("MODEL_NAME");
        }
    }
}
