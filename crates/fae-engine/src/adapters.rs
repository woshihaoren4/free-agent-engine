//! Generation-bound protocol adapters, separate from core execution contracts.
use crate::Engine;
use fae_agent::plugin::bridge::BridgeRegistry;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct EngineAdapters {
    pub engine: Engine,
    pub bridges: Arc<BridgeRegistry>,
}

impl EngineAdapters {
    pub async fn execute_runtime(
        &self,
        ctx: fae_agent::Ctx,
        id: &str,
        operation: fae_agent::plugin::bridge::RuntimeOperation,
        payload: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        use fae_agent::plugin::bridge::RuntimeOperation;
        let ty = self.bridges.public_runtime(id, operation)?;
        let meta = fae_agent::RuntimeCallMeta {
            runtime_id: id.into(),
            ty,
        };
        match operation {
            RuntimeOperation::Exec => Ok(self
                .engine
                .execute_runtime::<_, serde_json::Value>(ctx, meta, payload)
                .await?
                .resp),
            RuntimeOperation::Select => {
                let result =
                    fae_agent::Engine::select_on(&self.engine, &ctx, meta, Box::new(payload))
                        .await?;
                result
                    .downcast::<serde_json::Value>()
                    .map(|v| *v)
                    .map_err(|_| anyhow::anyhow!("Runtime boundary returned a non-JSON result"))
            }
        }
    }
}

/// Native routes opt into serde individually; opaque native values stay typed.
pub fn register_native_bridges(builder: &mut crate::EngineBuilder) -> anyhow::Result<()> {
    use fae_agent::plugin::bridge::*;
    use fae_agent::*;
    fn contract<A, B>() -> RuntimeContract
    where
        A: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
        B: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    {
        RuntimeContract {
            request: ValueCodec::serde::<A>(),
            response: ValueCodec::serde::<B>(),
        }
    }
    for (id, ty, exec, select) in [
        (
            crate::ModelRuntime::<async_openai::config::OpenAIConfig>::ID,
            TaskType::Model,
            fae_agent::plugin::leases::model_contract(),
            contract::<(), ()>(),
        ),
        (
            crate::ToolsRuntime::ID,
            TaskType::Tool,
            fae_agent::plugin::leases::tool_contract(),
            contract::<String, serde_json::Value>(),
        ),
    ] {
        if builder.runtimes().contains_runtime(id) {
            builder.register_runtime_bridge(
                id,
                vec![
                    (ty.clone(), RuntimeOperation::Exec, exec, true),
                    (ty, RuntimeOperation::Select, select, true),
                ],
            )?;
        }
    }
    for (id, ty, exec, select) in [
        (
            crate::SessionRuntime::ID,
            TaskType::Session,
            contract::<SessionRequest, SessionResponse>(),
            contract::<SessionQuery, SessionResponse>(),
        ),
        (
            crate::UserMemoryRuntime::ID,
            TaskType::Memory,
            contract::<UserMemoryRequest, UserMemoryResponse>(),
            contract::<UserMemoryQuery, UserMemoryResponse>(),
        ),
        (
            crate::McpRuntime::ID,
            TaskType::Mcp,
            contract::<McpRequest, McpResponse>(),
            contract::<McpQuery, Vec<McpToolInfo>>(),
        ),
        (
            crate::SkillRuntime::ID,
            TaskType::Skill,
            contract::<(), ()>(),
            contract::<SkillQuery, Vec<SkillInfo>>(),
        ),
        (
            crate::CompressionRuntime::ID,
            TaskType::Any(COMPRESSION_TASK_TYPE.into()),
            contract::<WorkflowActionRequest, WorkflowActionResponse>(),
            contract::<(), ()>(),
        ),
    ] {
        if builder.runtimes().contains_runtime(id) {
            builder.register_runtime_bridge(
                id,
                vec![
                    (ty.clone(), RuntimeOperation::Exec, exec, true),
                    (ty, RuntimeOperation::Select, select, true),
                ],
            )?;
        }
    }
    let workflow_codec = ValueCodec::new::<WorkflowEnv>(
        |ctx, value| {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                workflow_id: String,
                input: serde_json::Value,
            }
            let input: Input = serde_json::from_value(value)?;
            Ok(WorkflowEnv::new_with_user_id(
                input.workflow_id,
                input.input,
                ctx.scope()?.user.user_id.clone(),
            )
            .0)
        },
        |_, env| Ok(serde_json::json!({"workflow_id":env.workflow_id, "input":env.input})),
    );
    if builder.contains_plan_builder(&to_plan_ty::<WorkflowEnv>()) {
        builder.register_plan_bridge(
            "workflow",
            to_plan_ty::<WorkflowEnv>(),
            workflow_codec.clone(),
        )?;
    }
    if builder
        .runtimes()
        .contains_runtime(crate::WorkflowRuntime::ID)
    {
        builder.register_runtime_bridge(
            crate::WorkflowRuntime::ID,
            vec![
                (
                    TaskType::Workflow,
                    RuntimeOperation::Exec,
                    RuntimeContract {
                        request: workflow_codec,
                        response: ValueCodec::serde::<serde_json::Value>(),
                    },
                    true,
                ),
                (
                    TaskType::Workflow,
                    RuntimeOperation::Select,
                    contract::<String, WorkflowMetadata>(),
                    true,
                ),
            ],
        )?;
    }
    if builder.contains_plan_builder(&to_plan_ty::<SingleAgentEnv>()) {
        let codec = ValueCodec::new::<SingleAgentEnv>(
            |ctx, value| {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Input {
                    agent_id: String,
                    input: crate::adapters::AgentBoundaryInput,
                    #[serde(default)]
                    mode: Option<String>,
                }
                let input: Input = serde_json::from_value(value)?;
                let mut images = Vec::new();
                let text = match input.input {
                    AgentBoundaryInput::Text(text) => text,
                    AgentBoundaryInput::Content(content) => {
                        let mut texts = Vec::new();
                        for block in content {
                            match block {
                                protocol::ContentBlock::Text { text } => texts.push(text),
                                protocol::ContentBlock::Resource { resource } => {
                                    anyhow::ensure!(
                                        resource.media_type.starts_with("image/"),
                                        "Agent input supports image resources"
                                    );
                                    let uri = resource
                                        .uri
                                        .strip_prefix("file://")
                                        .unwrap_or(&resource.uri);
                                    if !["https://", "http://", "data:"]
                                        .iter()
                                        .any(|scheme| uri.starts_with(scheme))
                                    {
                                        anyhow::ensure!(
                                            std::path::Path::new(uri).is_absolute(),
                                            "Resource path must be absolute"
                                        );
                                        std::fs::File::open(uri)?;
                                    }
                                    images.push(uri.to_owned());
                                }
                                _ => anyhow::bail!("Agent input requires text or image resources"),
                            }
                        }
                        texts.join("\n")
                    }
                };
                anyhow::ensure!(
                    !text.trim().is_empty() || !images.is_empty(),
                    "Agent input cannot be empty"
                );
                anyhow::ensure!(
                    input.mode.as_deref().is_none_or(|mode| mode == "todo"),
                    "unsupported Agent mode"
                );
                let scope = ctx.scope()?;
                let (mut env, _) = SingleAgentEnv::from_agent_id_with_user_id(
                    input.agent_id,
                    text,
                    scope.user.user_id.clone(),
                );
                env = env.with_images(images);
                env.mode = input.mode;
                if let Some(binding) = &scope.execution {
                    env = env.with_session_id(binding.session.key().session_id.to_string());
                }
                Ok(env)
            },
            |_, env| {
                let SingleAgentSource::AgentId(agent_id) = env.source else {
                    anyhow::bail!("path-based Agent environment has no public encoding");
                };
                let input = if env.images.is_empty() {
                    serde_json::Value::String(env.input)
                } else {
                    let mut content = vec![serde_json::json!({"type":"text", "text":env.input})];
                    for uri in env.images {
                        let media_type = if uri.starts_with("data:") {
                            uri[5..].split(';').next().unwrap_or("image/png").to_owned()
                        } else {
                            match std::path::Path::new(&uri)
                                .extension()
                                .and_then(|s| s.to_str())
                            {
                                Some("jpg" | "jpeg") => "image/jpeg",
                                Some("webp") => "image/webp",
                                Some("gif") => "image/gif",
                                _ => "image/png",
                            }
                            .into()
                        };
                        content.push(serde_json::json!({"type":"resource", "resource":{
                            "uri":uri, "media_type":media_type, "name":null
                        }}));
                    }
                    serde_json::Value::Array(content)
                };
                Ok(serde_json::json!({"agent_id":agent_id, "input":input, "mode":env.mode}))
            },
        );
        builder.register_plan_bridge("single_agent", to_plan_ty::<SingleAgentEnv>(), codec)?;
    }
    Ok(())
}
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum AgentBoundaryInput {
    Text(String),
    Content(fae_agent::protocol::Content),
}
