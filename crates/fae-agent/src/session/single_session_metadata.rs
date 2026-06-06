use crate::SessionMetadata;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SingleSessionMD {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub additional_tips: String,
    #[serde(skip)]
    pub extend: HashMap<String, String>,
}
impl SingleSessionMD {
    pub fn set_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }
    pub fn get_id(&self) -> &str {
        self.id.as_str()
    }
    pub fn set_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = user_id.into();
        self
    }
    pub fn set_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
    pub fn get_name(&self) -> &str {
        self.name.as_str()
    }

    pub fn set_additional_tips(mut self, additional_tips: impl Into<String>) -> Self {
        self.additional_tips = additional_tips.into();
        self
    }
    pub fn get_additional_tips(&self) -> &str {
        self.additional_tips.as_str()
    }
    pub fn default_tips() -> String {
        let mut info = "\n---\n## The Project Metadata:".to_string();
        if let Ok(work_dir) = std::env::current_dir() {
            info.push_str(format!("\n - The path to the project you are currently working on is: $PROJECT_DIR=`{}`", work_dir.display()).as_str());
        } else {
            info.push_str("\n - Your current working directory is not available.");
        }
        info.push_str("\n - Note: All your commands, operations, file management, and memorization are performed within this directory: $PROJECT_DIR");
        info
    }
    pub fn set<K: Into<String>, V: Into<String>>(mut self, key: K, value: V) -> Self {
        self.extend.insert(key.into(), value.into());
        self
    }
    pub fn get(&self, key: &str) -> Option<&String> {
        self.extend.get(key)
    }
}

impl Default for SingleSessionMD {
    fn default() -> Self {
        Self {
            id: "main_session_id_1".to_string(),
            user_id: "master".to_string(),
            name: String::new(),
            additional_tips: "".to_string(),
            extend: HashMap::new(),
        }
    }
}

impl SessionMetadata for SingleSessionMD {
    fn id(&self) -> &str {
        self.id.as_str()
    }

    fn user_id(&self) -> &str {
        self.user_id.as_str()
    }

    fn additional_tips(&self) -> Option<String> {
        if self.additional_tips.is_empty() {
            Some(Self::default_tips())
        } else {
            Some(self.additional_tips.to_string())
        }
    }
    fn extend(&self) -> Option<HashMap<String, String>> {
        Some(self.extend.clone())
    }
}
