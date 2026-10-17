use super::*;

#[derive(Debug, Clone)]
pub(in crate::plan::single_agent) struct ResolvedSubAgent {
    pub(in crate::plan::single_agent) agent_id: String,
    pub(in crate::plan::single_agent) desc: String,
    pub(in crate::plan::single_agent) source: SingleAgentSource,
}

#[derive(Debug, Clone)]
pub(in crate::plan::single_agent) struct ResolvedWorkflow {
    pub(in crate::plan::single_agent) workflow_id: String,
    pub(in crate::plan::single_agent) desc: String,
}

pub(in crate::plan::single_agent) fn sub_agent_tool_definition(
    agents: &[ResolvedSubAgent],
) -> ChatCompletionTools {
    ChatCompletionTools::Function(async_openai::types::chat::ChatCompletionTool {
        function: FunctionObject {
            name: AGENT_TOOL_NAME.to_string(),
            description: Some("Delegate a task to a configured sub-agent.".to_string()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "enum": agents
                            .iter()
                            .map(|agent| agent.agent_id.clone())
                            .collect::<Vec<_>>()
                    },
                    "input": {
                        "type": "string",
                        "description": "The complete task and context for the sub-agent."
                    }
                },
                "required": ["agent_id", "input"],
                "additionalProperties": false
            })),
            strict: Some(true),
        },
    })
}

pub(in crate::plan::single_agent) fn workflow_tool_definition(
    workflows: &[ResolvedWorkflow],
) -> ChatCompletionTools {
    ChatCompletionTools::Function(async_openai::types::chat::ChatCompletionTool {
        function: FunctionObject {
            name: WORKFLOW_TOOL_NAME.to_string(),
            description: Some("Run a configured workflow and return its final JSON output.".into()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "workflow_id": {
                        "type": "string",
                        "enum": workflows
                            .iter()
                            .map(|workflow| workflow.workflow_id.clone())
                            .collect::<Vec<_>>()
                    },
                    "input": {
                        "description": "JSON input passed to the workflow."
                    }
                },
                "required": ["workflow_id"],
                "additionalProperties": false
            })),
            strict: Some(true),
        },
    })
}

#[derive(Debug, Clone)]
pub(in crate::plan) enum CallableRoute {
    Tool(String),
    Mcp {
        server: String,
        tool_name: String,
    },
    Agent {
        sources: HashMap<String, SingleAgentSource>,
    },
    Workflow {
        workflow_ids: HashSet<String>,
    },
    Orchestrated {
        message: String,
    },
}
