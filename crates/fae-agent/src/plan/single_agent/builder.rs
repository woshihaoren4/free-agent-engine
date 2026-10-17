use super::*;

#[derive(Clone)]
pub struct SingleAgentPlanBuilder {
    home_dir: PathBuf,
    hooks: Vec<Arc<dyn SingleAgentHookBuilder>>,
}

impl std::fmt::Debug for SingleAgentPlanBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleAgentPlanBuilder")
            .field("home_dir", &self.home_dir)
            .field("hook_count", &self.hooks.len())
            .finish()
    }
}

impl Default for SingleAgentPlanBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SingleAgentPlanBuilder {
    pub fn new() -> Self {
        Self::with_home_dir(default_fae_home())
    }

    pub fn with_home_dir(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
            hooks: Vec::new(),
        }
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub fn hooks(&self) -> &[Arc<dyn SingleAgentHookBuilder>] {
        &self.hooks
    }

    pub fn add_hook<H>(&mut self, builder: H)
    where
        H: SingleAgentHookBuilder,
    {
        self.hooks.push(Arc::new(builder));
    }

    pub fn add_hook_arc(&mut self, builder: Arc<dyn SingleAgentHookBuilder>) {
        self.hooks.push(builder);
    }

    pub fn remove_hooks(&mut self) -> Vec<Arc<dyn SingleAgentHookBuilder>> {
        std::mem::take(&mut self.hooks)
    }

    pub async fn load_config(
        &self,
        source: &SingleAgentSource,
    ) -> anyhow::Result<(SingleAgentConfig, String)> {
        let (config_path, prompt_path, expected_agent_id) = self.source_paths(source)?;

        let config_bytes = tokio::fs::read(&config_path).await.map_err(|error| {
            anyhow::anyhow!(
                "load single-agent config `{}`: {error}",
                config_path.display()
            )
        })?;
        let config: SingleAgentConfig = serde_json::from_slice(&config_bytes).map_err(|error| {
            anyhow::anyhow!(
                "parse single-agent config `{}`: {error}",
                config_path.display()
            )
        })?;
        validate_config(&config)?;
        if let Some(agent_id) = expected_agent_id {
            anyhow::ensure!(
                config.agent.name == agent_id,
                "agent config `{}` contains name `{}`, expected `{agent_id}`",
                config_path.display(),
                config.agent.name
            );
        }
        let prompt = tokio::fs::read_to_string(&prompt_path)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "load single-agent prompt `{}`: {error}",
                    prompt_path.display()
                )
            })?;
        Ok((config, prompt))
    }

    fn source_paths<'a>(
        &self,
        source: &'a SingleAgentSource,
    ) -> anyhow::Result<(PathBuf, PathBuf, Option<&'a str>)> {
        Ok(match source {
            SingleAgentSource::AgentId(agent_id) => {
                validate_agent_id(agent_id)?;
                let agents_dir = self.home_dir.join("agents");
                (
                    agents_dir.join(format!("{agent_id}_config.json")),
                    agents_dir.join(format!("{agent_id}_prompt.txt")),
                    Some(agent_id.as_str()),
                )
            }
            SingleAgentSource::Paths { config, prompt } => (config.clone(), prompt.clone(), None),
        })
    }

    pub(super) async fn build_hook(&self) -> Arc<dyn SingleAgentHook> {
        let mut hooks = Vec::with_capacity(self.hooks.len());
        for builder in &self.hooks {
            hooks.push(builder.build().await);
        }
        Arc::new(CompositeSingleAgentHook::new(hooks))
    }
}

#[async_trait::async_trait]
impl PlanBuilderWithEnv<SingleAgentEnv> for SingleAgentPlanBuilder {
    async fn build(&self, rt: RT, ctx: Ctx, env: SingleAgentEnv) -> anyhow::Result<Box<dyn Plan>> {
        anyhow::ensure!(
            !env.input.trim().is_empty() || !env.images.is_empty(),
            "input and images cannot both be empty"
        );
        anyhow::ensure!(!env.user_id.trim().is_empty(), "user_id cannot be empty");
        env.session.set_user_id(env.user_id.clone());
        let mode = SingleAgentMode::parse(env.mode.as_deref())?;
        let (config_path, prompt_path, _) = self.source_paths(&env.source)?;
        let (mut config, base_prompt) = self.load_config(&env.source).await?;
        config.agent.user_id = env.user_id.clone();
        anyhow::ensure!(
            !env.ancestor_agents.contains(&config.agent.name),
            "recursive sub-agent call detected for `{}`",
            config.agent.name
        );
        let mut ancestor_agents = env.ancestor_agents;
        ancestor_agents.push(config.agent.name.clone());
        if let Some(session_id) = env.session_id {
            anyhow::ensure!(!session_id.trim().is_empty(), "session_id cannot be empty");
            config.agent.session_id = session_id;
        }
        let skills = resolve_skills(&rt, &config.skills).await?;
        let sub_agents = self
            .resolve_sub_agents(&config.agent.name, &config.sub_agents)
            .await?;
        let workflows = resolve_workflows(&rt, &config.workflows).await?;
        let configured_tools = config
            .tools
            .iter()
            .filter(|tool_name| !matches!(tool_name.as_str(), AGENT_TOOL_NAME | WORKFLOW_TOOL_NAME))
            .cloned()
            .collect::<Vec<_>>();
        let (mut tool_definitions, mut tool_routes) = resolve_tools(&rt, &configured_tools).await?;
        if mode == Some(SingleAgentMode::Todo) {
            anyhow::ensure!(
                matches!(
                    tool_routes.get(TODO_TOOL_NAME),
                    Some(CallableRoute::Tool(_))
                ),
                "single-agent todo mode requires the `todo` tool"
            );
        }
        let (mcp_definitions, mcp_routes, mcp_tools) =
            resolve_mcp_tools(&rt, &config.mcp_servers).await?;
        for (name, route) in mcp_routes {
            anyhow::ensure!(
                tool_routes.insert(name.clone(), route).is_none(),
                "multiple configured tools expose the model name `{name}`"
            );
        }
        tool_definitions.extend(mcp_definitions);
        if !sub_agents.is_empty() {
            anyhow::ensure!(
                tool_routes
                    .insert(
                        AGENT_TOOL_NAME.to_string(),
                        CallableRoute::Agent {
                            sources: sub_agents
                                .iter()
                                .map(|agent| (agent.agent_id.clone(), agent.source.clone()))
                                .collect(),
                        },
                    )
                    .is_none(),
                "configured tool name `{AGENT_TOOL_NAME}` conflicts with the sub-agent tool"
            );
            tool_definitions.push(sub_agent_tool_definition(&sub_agents));
        }
        if !workflows.is_empty() {
            anyhow::ensure!(
                tool_routes
                    .insert(
                        WORKFLOW_TOOL_NAME.to_string(),
                        CallableRoute::Workflow {
                            workflow_ids: workflows
                                .iter()
                                .map(|workflow| workflow.workflow_id.clone())
                                .collect(),
                        },
                    )
                    .is_none(),
                "configured tool name `{WORKFLOW_TOOL_NAME}` conflicts with the workflow tool"
            );
            tool_definitions.push(workflow_tool_definition(&workflows));
        }
        let prompt = build_prompt(
            &base_prompt,
            &config.prompt_sections,
            &skills,
            &mcp_tools,
            &sub_agents,
            &workflows,
            Some(&SingleAgentRuntimeContext::new(
                &config.agent,
                &self.home_dir,
                &config_path,
                &prompt_path,
            )),
        )?;
        let hook = self.build_hook().await;
        let template = SingleAgentTemplate {
            agent: config.agent,
            prompt,
            model: config.model,
            tool_definitions,
            tool_routes,
            ancestor_agents,
            hook,
        };
        let binding = SingleAgentBinding {
            rt: rt.clone(),
            ctx: ctx.clone(),
            template: template.clone(),
            mode,
        };
        let turn_id = env.session.bind(binding).await?;

        Ok(build_single_agent_plan(
            ctx,
            template,
            env.input,
            env.images,
            turn_id,
            env.session,
            mode,
        ))
    }
}

impl SingleAgentPlanBuilder {
    pub(super) async fn resolve_sub_agents(
        &self,
        parent_agent_name: &str,
        agent_ids: &[String],
    ) -> anyhow::Result<Vec<ResolvedSubAgent>> {
        let mut agents = Vec::with_capacity(agent_ids.len());
        let mut seen = std::collections::HashSet::with_capacity(agent_ids.len());
        for agent_id in agent_ids {
            validate_agent_id(agent_id)?;
            anyhow::ensure!(
                agent_id != parent_agent_name,
                "agent `{parent_agent_name}` cannot mount itself as a sub-agent"
            );
            anyhow::ensure!(
                seen.insert(agent_id.clone()),
                "sub-agent `{agent_id}` is configured more than once"
            );
            let source = SingleAgentSource::AgentId(agent_id.clone());
            let (config, _) = self.load_config(&source).await.map_err(|error| {
                anyhow::anyhow!("load sub-agent `{agent_id}` configuration: {error}")
            })?;
            anyhow::ensure!(
                !config.agent.desc.trim().is_empty(),
                "sub-agent `{agent_id}` description cannot be empty"
            );
            let agents_dir = self.home_dir.join("agents");
            agents.push(ResolvedSubAgent {
                agent_id: agent_id.clone(),
                desc: config.agent.desc,
                source: SingleAgentSource::Paths {
                    config: agents_dir.join(format!("{agent_id}_config.json")),
                    prompt: agents_dir.join(format!("{agent_id}_prompt.txt")),
                },
            });
        }
        Ok(agents)
    }
}

mod config;
mod prompt;
mod tools;

pub(super) use config::*;
pub(super) use prompt::*;
pub(in crate::plan) use tools::CallableRoute;
pub(super) use tools::{
    ResolvedSubAgent, ResolvedWorkflow, sub_agent_tool_definition, workflow_tool_definition,
};
