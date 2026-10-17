use super::*;

pub(in crate::plan::single_agent) fn validate_config(
    config: &SingleAgentConfig,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !config.agent.name.trim().is_empty(),
        "agent name cannot be empty"
    );
    anyhow::ensure!(
        !config.agent.user_id.trim().is_empty(),
        "user_id cannot be empty"
    );
    anyhow::ensure!(
        !config.agent.session_id.trim().is_empty(),
        "session_id cannot be empty"
    );
    anyhow::ensure!(
        !config.model.model.trim().is_empty(),
        "model cannot be empty"
    );
    anyhow::ensure!(
        config.model.trigger_compression_size > 0,
        "trigger_compression_size must be positive"
    );
    anyhow::ensure!(
        config.model.max_tool_iterations > 0,
        "max_tool_iterations must be positive"
    );
    for section in &config.prompt_sections {
        validate_prompt_tag(&section.tag)?;
    }
    validate_workflow_ids(&config.workflows)?;
    Ok(())
}

pub(in crate::plan::single_agent) fn validate_agent_id(agent_id: &str) -> anyhow::Result<()> {
    let mut components = Path::new(agent_id).components();
    anyhow::ensure!(
        !agent_id.is_empty()
            && matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none(),
        "agent id must be a single non-empty path component"
    );
    Ok(())
}

pub(in crate::plan::single_agent) fn validate_workflow_ids(
    workflow_ids: &[String],
) -> anyhow::Result<()> {
    let mut seen = HashSet::with_capacity(workflow_ids.len());
    for workflow_id in workflow_ids {
        let mut components = Path::new(workflow_id).components();
        anyhow::ensure!(
            !workflow_id.is_empty()
                && matches!(components.next(), Some(Component::Normal(_)))
                && components.next().is_none(),
            "workflow id must be a single non-empty path component"
        );
        anyhow::ensure!(
            seen.insert(workflow_id),
            "workflow `{workflow_id}` is configured more than once"
        );
    }
    Ok(())
}

pub(in crate::plan::single_agent) fn default_fae_home() -> PathBuf {
    match std::env::var_os("FAE_HOST") {
        Some(home) if !home.is_empty() => expand_home(PathBuf::from(home)),
        _ => dirs::home_dir()
            .map(|home| home.join(".fae"))
            .unwrap_or_else(|| PathBuf::from(".fae")),
    }
}

pub(in crate::plan::single_agent) fn expand_home(path: PathBuf) -> PathBuf {
    let Some(path_str) = path.to_str() else {
        return path;
    };
    if path_str == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    if let Some(rest) = path_str.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path
}

pub(in crate::plan::single_agent) async fn resolve_tools(
    rt: &RT,
    tools: &[String],
) -> anyhow::Result<(Vec<ChatCompletionTools>, HashMap<String, CallableRoute>)> {
    let mut definitions = Vec::with_capacity(tools.len());
    let mut routes = HashMap::with_capacity(tools.len());
    for tool_name in tools {
        let value = rt
            .select::<_, Value>(TaskType::Tool, tool_name.clone())
            .await?;
        let function = serde_json::from_value::<FunctionObject>(value).map_err(|error| {
            anyhow::anyhow!("invalid description for tool `{tool_name}`: {error}")
        })?;
        anyhow::ensure!(
            routes
                .insert(
                    function.name.clone(),
                    CallableRoute::Tool(tool_name.clone())
                )
                .is_none(),
            "multiple configured tools expose the model name `{}`",
            function.name
        );
        definitions.push(ChatCompletionTools::Function(
            async_openai::types::chat::ChatCompletionTool { function },
        ));
    }
    Ok((definitions, routes))
}

pub(in crate::plan::single_agent) async fn resolve_skills(
    rt: &RT,
    queries: &[SkillQuery],
) -> anyhow::Result<Vec<SkillInfo>> {
    let mut skills = Vec::new();
    for query in queries {
        let mut found = rt
            .select::<_, Vec<SkillInfo>>(TaskType::Skill, query.clone())
            .await?;
        skills.append(&mut found);
    }
    Ok(skills)
}

pub(in crate::plan::single_agent) async fn resolve_workflows(
    rt: &RT,
    workflow_ids: &[String],
) -> anyhow::Result<Vec<ResolvedWorkflow>> {
    let mut workflows = Vec::with_capacity(workflow_ids.len());
    for workflow_id in workflow_ids {
        let metadata = rt
            .select::<_, WorkflowMetadata>(TaskType::Workflow, workflow_id.clone())
            .await
            .map_err(|error| anyhow::anyhow!("load workflow `{workflow_id}`: {error}"))?;
        anyhow::ensure!(
            metadata.id == *workflow_id,
            "workflow `{workflow_id}` resolved to metadata for `{}`",
            metadata.id
        );
        workflows.push(ResolvedWorkflow {
            workflow_id: metadata.id,
            desc: metadata.desc,
        });
    }
    Ok(workflows)
}

pub(in crate::plan::single_agent) fn validate_prompt_tag(tag: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !tag.is_empty()
            && tag
                .bytes()
                .enumerate()
                .all(|(index, byte)| byte.is_ascii_lowercase()
                    || (index > 0 && (byte.is_ascii_digit() || byte == b'_'))),
        "prompt section tag `{tag}` must use lowercase English letters, digits, or underscores and start with a letter"
    );
    Ok(())
}

pub(in crate::plan::single_agent) fn append_prompt_section(
    prompt: &mut String,
    tag: &str,
    text: impl AsRef<str>,
) -> anyhow::Result<()> {
    validate_prompt_tag(tag)?;
    let text = text.as_ref().trim();
    if text.is_empty() {
        return Ok(());
    }
    if !prompt.is_empty() {
        prompt.push_str("\n\n");
    }
    prompt.push('<');
    prompt.push_str(tag);
    prompt.push_str(">\n");
    prompt.push_str(text);
    prompt.push_str("\n</");
    prompt.push_str(tag);
    prompt.push('>');
    Ok(())
}
