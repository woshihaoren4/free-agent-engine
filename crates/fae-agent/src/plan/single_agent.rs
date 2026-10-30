use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex, RwLock as StdRwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Context;
use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCallChunk,
    ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessage,
    ChatCompletionRequestAssistantMessageContent, ChatCompletionRequestMessage,
    ChatCompletionRequestMessageContentPartImage, ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestSystemMessage, ChatCompletionRequestToolMessage,
    ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart,
    ChatCompletionTools, CreateChatCompletionRequest, FinishReason, FunctionCall, FunctionObject,
    ImageDetail, ImageUrl,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Notify, RwLock};
use tokio_stream::StreamExt;
use wd_tools::Base64StdEncode;

use super::single_agent_todo::{SingleAgentMode, SingleAgentTodoPlan};
use crate::{
    CompositeSingleAgentHook, Ctx, McpQuery, McpRequest, McpResponse, McpToolInfo, ModelResponse,
    Plan, PlanBuilderWithEnv, PlanNext, RT, Session, SessionEvent, SessionEventData, SessionInput,
    SessionInputData, SessionMessage, SessionMessageRole, SessionOutput, SessionOutputChannel,
    SessionRequest, SessionResponse, SingleAgentHook, SingleAgentHookBuilder,
    SingleAgentHookContext, SkillInfo, SkillQuery, TaskMeta, TaskReq, TaskRequest, TaskResp,
    TaskResponse, TaskType, ToolInvocation, ToolRequest, ToolRespItem, ToolResponse, UserMemory,
    UserMemoryRequest, UserMemoryResponse, WorkflowActionRequest, WorkflowActionResponse,
    WorkflowMetadata,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleAgentInfo {
    pub name: String,
    #[serde(default)]
    pub desc: String,
    pub user_id: String,
    pub session_id: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleAgentModelConfig {
    pub model: String,
    #[serde(default)]
    pub image_detail: ImageDetail,
    #[serde(default = "default_trigger_compression_size")]
    pub trigger_compression_size: usize,
    #[serde(default = "default_history_turns")]
    pub history_turns: usize,
    #[serde(default = "default_max_completion_tokens")]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleAgentConfig {
    pub agent: SingleAgentInfo,
    pub model: SingleAgentModelConfig,
    #[serde(default)]
    pub prompt_sections: Vec<PromptSection>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub skills: Vec<SkillQuery>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub sub_agents: Vec<String>,
    #[serde(default)]
    pub workflows: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSection {
    pub tag: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SingleAgentSource {
    AgentId(String),
    Paths { config: PathBuf, prompt: PathBuf },
}

const fn default_max_tool_iterations() -> usize {
    128
}

const fn default_trigger_compression_size() -> usize {
    32_000
}

const fn default_history_turns() -> usize {
    DEFAULT_HISTORY_TURNS
}

const fn default_max_completion_tokens() -> Option<u32> {
    Some(32_000)
}

const MAX_EMPTY_MODEL_RETRIES: usize = 1;
const EMPTY_MODEL_RETRY_PROMPT: &str = "Your previous response ended without assistant output. \
Continue the task from the available context and return either a tool call or a final answer.";
const IMAGE_TOKEN_COST_ENV: &str = "FAE_IMAGE_TOKEN_COST";
const DEFAULT_IMAGE_TOKEN_COST: usize = 1_280;

pub const COMPRESSION_TASK_TYPE: &str = "workflow.compression";
pub const DEFAULT_HISTORY_TURNS: usize = 10;
pub const DEFAULT_USER_ID: &str = "master";
const AGENT_TOOL_NAME: &str = "agent";
const WORKFLOW_TOOL_NAME: &str = "workflow";
#[cfg(test)]
const MEMORY_UPDATE_TOOL_NAME: &str = "memory_update";
const TODO_TOOL_NAME: &str = "todo";

#[derive(Debug, Clone)]
pub struct AgentToolInvocation {
    agent_id: String,
    source: SingleAgentSource,
    parent_agent_name: String,
    ancestor_agents: Vec<String>,
}

impl AgentToolInvocation {
    fn for_sub_agent(
        agent_id: String,
        source: SingleAgentSource,
        parent_agent_name: String,
        ancestor_agents: Vec<String>,
    ) -> Self {
        Self {
            agent_id,
            source,
            parent_agent_name,
            ancestor_agents,
        }
    }

    pub fn into_env(
        self,
        ctx: &Ctx,
        agent_id: &str,
        input: String,
    ) -> anyhow::Result<(SingleAgentEnv, CommonSession)> {
        anyhow::ensure!(
            self.agent_id == agent_id,
            "agent `{agent_id}` is not configured"
        );
        let (env, session) = SingleAgentEnv::new_with_parent_agent(
            self.source,
            input,
            ctx.scope()?
                .session
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("agent invocation requires caller session"))?
                .session
                .clone(),
            self.parent_agent_name,
        );
        Ok((env.with_ancestor_agents(self.ancestor_agents), session))
    }
}

#[derive(Debug)]
pub struct SingleAgentEnv {
    pub source: SingleAgentSource,
    pub input: String,
    pub images: Vec<String>,
    pub user_id: String,
    /// `None` or an empty string uses the standard flow; `"todo"` enables todo orchestration.
    pub mode: Option<String>,
    session_id: Option<String>,
    ancestor_agents: Vec<String>,
    session: CommonSession,
}

impl SingleAgentEnv {
    pub fn from_agent_id(
        agent_id: impl Into<String>,
        input: impl Into<String>,
    ) -> (Self, CommonSession) {
        Self::new(SingleAgentSource::AgentId(agent_id.into()), input)
    }

    pub fn from_agent_id_with_user_id(
        agent_id: impl Into<String>,
        input: impl Into<String>,
        user_id: impl Into<String>,
    ) -> (Self, CommonSession) {
        Self::new_with_user_id(SingleAgentSource::AgentId(agent_id.into()), input, user_id)
    }

    pub fn from_paths(
        config: impl Into<PathBuf>,
        prompt: impl Into<PathBuf>,
        input: impl Into<String>,
    ) -> (Self, CommonSession) {
        Self::new(
            SingleAgentSource::Paths {
                config: config.into(),
                prompt: prompt.into(),
            },
            input,
        )
    }

    pub fn new(source: SingleAgentSource, input: impl Into<String>) -> (Self, CommonSession) {
        Self::new_with_user_id(source, input, DEFAULT_USER_ID)
    }

    pub fn new_with_user_id(
        source: SingleAgentSource,
        input: impl Into<String>,
        user_id: impl Into<String>,
    ) -> (Self, CommonSession) {
        let user_id = user_id.into();
        let session = CommonSession::new();
        session.set_user_id(user_id.clone());
        (
            Self {
                source,
                input: input.into(),
                images: Vec::new(),
                user_id,
                mode: None,
                session_id: None,
                ancestor_agents: Vec::new(),
                session: session.clone(),
            },
            session,
        )
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        let user_id = user_id.into();
        self.user_id = user_id.clone();
        self.session.set_user_id(user_id);
        self
    }

    pub fn with_mode(mut self, mode: impl Into<String>) -> Self {
        self.mode = Some(mode.into());
        self
    }

    pub fn with_images(mut self, images: Vec<String>) -> Self {
        self.images = images;
        self
    }

    fn with_ancestor_agents(mut self, ancestor_agents: Vec<String>) -> Self {
        self.ancestor_agents = ancestor_agents;
        self
    }

    pub fn session(&self) -> CommonSession {
        self.session.clone()
    }

    pub fn new_with_session(
        source: SingleAgentSource,
        input: impl Into<String>,
        workflow_session: CommonSession,
        workflow_id: impl Into<String>,
        node_id: impl Into<String>,
    ) -> (Self, CommonSession) {
        let session = CommonSession::new_in_workflow(workflow_session, workflow_id, node_id);
        let user_id = session.user_id();
        (
            Self {
                source,
                input: input.into(),
                images: Vec::new(),
                user_id,
                mode: None,
                session_id: None,
                ancestor_agents: Vec::new(),
                session: session.clone(),
            },
            session,
        )
    }

    pub fn new_with_parent_agent(
        source: SingleAgentSource,
        input: impl Into<String>,
        parent_session: CommonSession,
        parent_agent_name: impl Into<String>,
    ) -> (Self, CommonSession) {
        let session = CommonSession::new_in_agent(parent_session, parent_agent_name);
        let user_id = session.user_id();
        (
            Self {
                source,
                input: input.into(),
                images: Vec::new(),
                user_id,
                mode: None,
                session_id: None,
                ancestor_agents: Vec::new(),
                session: session.clone(),
            },
            session,
        )
    }
}

mod builder;
mod execution;
mod message;
mod session;

pub(in crate::plan) use builder::CallableRoute;
pub use builder::SingleAgentPlanBuilder;
pub(in crate::plan) use execution::{SingleAgentPlan, SingleAgentTemplate};
pub use session::CommonSession;

use builder::*;
use execution::*;
use message::*;
