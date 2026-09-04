use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{SessionRequest, SingleAgentInfo, SingleAgentModelConfig, SkillQuery};

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

fn deserialize_targets<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(target) => vec![target],
        OneOrMany::Many(targets) => targets,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowMetadata {
    #[serde(default = "workflow_version")]
    pub version: u32,
    pub id: String,
    pub nodes: BTreeMap<String, WorkflowNode>,
}

impl WorkflowMetadata {
    pub fn validate(&self) -> anyhow::Result<()> {
        crate::WorkflowMetadataBuilder::validate_metadata(self)
    }

    pub fn to_json(&self) -> anyhow::Result<String> {
        self.validate()?;
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        json.parse()
    }

    pub async fn save_json(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        tokio::fs::write(path, self.to_json()?).await?;
        Ok(())
    }

    pub async fn load_json(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::from_json(&tokio::fs::read_to_string(path).await?)
    }
}

impl fmt::Display for WorkflowMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let serialized = serde_json::to_string(self).map_err(|_| fmt::Error)?;
        formatter.write_str(&serialized)
    }
}

impl FromStr for WorkflowMetadata {
    type Err = anyhow::Error;

    fn from_str(serialized: &str) -> Result<Self, Self::Err> {
        let metadata: Self = serde_json::from_str(serialized)?;
        metadata.validate()?;
        Ok(metadata)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowNode {
    Start {
        #[serde(deserialize_with = "deserialize_targets")]
        next: Vec<String>,
    },
    ParallelStart {
        next: Vec<String>,
    },
    End {
        #[serde(default)]
        output: Option<Value>,
    },
    JoinEnd {
        #[serde(default)]
        output: Option<Value>,
    },
    Execute {
        action: WorkflowAction,
        #[serde(deserialize_with = "deserialize_targets")]
        next: Vec<String>,
    },
    Decision {
        condition: WorkflowCondition,
        #[serde(deserialize_with = "deserialize_targets")]
        on_true: Vec<String>,
        #[serde(deserialize_with = "deserialize_targets")]
        on_false: Vec<String>,
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
            Self::Start { next } | Self::ParallelStart { next } | Self::Execute { next, .. } => {
                next.iter().map(String::as_str).collect()
            }
            Self::End { .. } | Self::JoinEnd { .. } => Vec::new(),
            Self::Decision {
                on_true, on_false, ..
            } => on_true.iter().chain(on_false).map(String::as_str).collect(),
            Self::Loop { body, next, .. } => vec![body, next],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowAction {
    Workflow {
        workflow_id: String,
        #[serde(default)]
        input: Value,
    },
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
        #[serde(default)]
        skills: Vec<SkillQuery>,
        #[serde(default)]
        mcp_servers: Vec<String>,
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
    use crate::WorkflowMetadataBuilder;
    use serde_json::json;

    #[tokio::test]
    async fn saves_and_loads_a_valid_workflow() {
        let mut builder = WorkflowMetadataBuilder::new("persisted");
        builder.start("start", "end").unwrap();
        builder.end("end", Some(json!("{$input.result}"))).unwrap();
        let metadata = builder.build().unwrap();
        let path = std::env::temp_dir().join(format!("fae-workflow-{}.json", wd_tools::uuid::v4()));

        metadata.save_json(&path).await.unwrap();
        let loaded = WorkflowMetadata::load_json(&path).await.unwrap();
        tokio::fs::remove_file(path).await.unwrap();

        assert_eq!(
            serde_json::to_value(loaded).unwrap(),
            serde_json::to_value(metadata).unwrap()
        );
    }

    #[test]
    fn serializes_to_and_parses_from_string() {
        let mut builder = WorkflowMetadataBuilder::new("string-round-trip");
        builder.start("start", "child").unwrap();
        builder
            .execute(
                "child",
                WorkflowAction::Workflow {
                    workflow_id: "child".to_string(),
                    input: json!({
                        "value": "{$input.value}"
                    }),
                },
                "end",
            )
            .unwrap();
        builder.end("end", None).unwrap();
        let metadata = builder.build().unwrap();

        let serialized = metadata.to_string();
        let parsed: WorkflowMetadata = serialized.parse().unwrap();

        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            serde_json::to_value(metadata).unwrap()
        );
    }

    #[test]
    fn parses_legacy_single_target_fields() {
        let metadata = WorkflowMetadata::from_json(
            r#"{
                "version": 1,
                "id": "legacy",
                "nodes": {
                    "start": {"type": "start", "next": "decision"},
                    "decision": {
                        "type": "decision",
                        "condition": {"type": "truthy", "value": true},
                        "on_true": "end",
                        "on_false": "end"
                    },
                    "end": {"type": "end"}
                }
            }"#,
        )
        .unwrap();

        assert!(matches!(
            metadata.nodes.get("start"),
            Some(WorkflowNode::Start { next }) if next == &["decision"]
        ));
        assert!(matches!(
            metadata.nodes.get("decision"),
            Some(WorkflowNode::Decision {
                on_true,
                on_false,
                ..
            }) if on_true == &["end"] && on_false == &["end"]
        ));
    }
}
