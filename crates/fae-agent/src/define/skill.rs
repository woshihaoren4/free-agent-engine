use std::collections::HashMap;
use std::fmt::Display;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug,Default,Serialize,Deserialize)]
pub struct SkillHeader{
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub metadata: Option<HashMap<String,Value>>,
    pub author: Option<String>,
    pub trigger: Option<String>,
    pub tags: Option<Vec<String>>,
}

impl SkillHeader {
    pub fn format(&self) -> String {
        serde_json::to_string(&self).unwrap_or(format!("{:?}",self))
    }
}

impl Display for SkillHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Ok(s) = serde_json::to_string(self) {
            write!(f, "{}", s)
        }else{
            write!(f, "{}: {}", self.name, self.description)
        }
    }
}