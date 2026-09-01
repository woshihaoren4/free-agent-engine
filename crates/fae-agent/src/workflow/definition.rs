use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{SessionRequest, SingleAgentInfo, SingleAgentModelConfig};

pub const WORKFLOW_VERSION: u32 = 1;

fn workflow_version() -> u32 {
    WORKFLOW_VERSION
}

fn default_max_iterations() -> usize {
    100
}

fn default_python_task_type() -> String {
    "workflow.python".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    #[serde(default = "workflow_version")]
    pub version: u32,
    pub id: String,
    pub nodes: BTreeMap<String, WorkflowNode>,
}

impl WorkflowDefinition {
    pub fn validate(&self) -> anyhow::Result<()> {
        crate::WorkflowBuilder::validate_definition(self)
    }

    pub fn to_json(&self) -> anyhow::Result<String> {
        self.validate()?;
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        let workflow: Self = serde_json::from_str(json)?;
        workflow.validate()?;
        Ok(workflow)
    }

    pub async fn save_json(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        tokio::fs::write(path, self.to_json()?).await?;
        Ok(())
    }

    pub async fn load_json(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::from_json(&tokio::fs::read_to_string(path).await?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowNode {
    Start {
        next: String,
    },
    End {
        #[serde(default)]
        output: Option<Value>,
    },
    Execute {
        action: WorkflowAction,
        next: String,
    },
    Decision {
        condition: WorkflowCondition,
        on_true: String,
        on_false: String,
    },
    Loop {
        condition: WorkflowCondition,
        body: String,
        next: String,
        #[serde(default = "default_max_iterations")]
        max_iterations: usize,
    },
}

impl WorkflowNode {
    pub(crate) fn successors(&self) -> Vec<&str> {
        match self {
            Self::Start { next } | Self::Execute { next, .. } => vec![next],
            Self::End { .. } => Vec::new(),
            Self::Decision {
                on_true, on_false, ..
            } => vec![on_true, on_false],
            Self::Loop { body, next, .. } => vec![body, next],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowAction {
    Tool {
        tool_name: String,
        #[serde(default)]
        arguments: Value,
    },
    SingleAgent {
        agent: SingleAgentInfo,
        prompt: String,
        model: SingleAgentModelConfig,
        input: Value,
        #[serde(default)]
        tools: Vec<String>,
    },
    Session {
        request: SessionRequest,
    },
    Python {
        code: String,
        #[serde(default)]
        arguments: Value,
        #[serde(default = "default_python_task_type")]
        task_type: String,
    },
    Custom {
        task_type: String,
        #[serde(default)]
        request: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowCondition {
    Truthy {
        value: Value,
    },
    Exists {
        value: Value,
    },
    Compare {
        left: Value,
        op: WorkflowCompare,
        right: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCompare {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub workflow: WorkflowDefinition,
    #[serde(default)]
    pub input: Value,
}

impl WorkflowRun {
    pub fn new(workflow: WorkflowDefinition, input: Value) -> Self {
        Self { workflow, input }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowActionRequest {
    pub action: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowActionResponse {
    #[serde(default)]
    pub output: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkflowBuilder;
    use serde_json::json;

    #[tokio::test]
    async fn saves_and_loads_a_valid_workflow() {
        let mut builder = WorkflowBuilder::new("persisted");
        builder.start("start", "end").unwrap();
        builder.end("end", Some(json!("{$input.result}"))).unwrap();
        let workflow = builder.build().unwrap();
        let path = std::env::temp_dir().join(format!("fae-workflow-{}.json", wd_tools::uuid::v4()));

        workflow.save_json(&path).await.unwrap();
        let loaded = WorkflowDefinition::load_json(&path).await.unwrap();
        tokio::fs::remove_file(path).await.unwrap();

        assert_eq!(
            serde_json::to_value(loaded).unwrap(),
            serde_json::to_value(workflow).unwrap()
        );
    }
}
