use serde::{Deserialize, Serialize};
use crate::SessionMetadata;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SingleSessionMD {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub additional_tips: String,
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
        let mut info = "\n---\n## Project Metadata:\n - Note that all user commands operate based on this directory.".to_string();
        if let Ok(work_dir) = std::env::current_dir() {
            info.push_str(format!("\n - Your working directory is as follows:{}", work_dir.display()).as_str());
        }else{
            info.push_str("\n - Your current working directory is not available.");
        }
        info
    }
}

impl Default for SingleSessionMD {
    fn default() -> Self {
        Self {
            id: "main_session_id_1".to_string(),
            user_id: "master".to_string(),
            name: String::new(),
            additional_tips: Self::default_tips(),
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

    fn additional_tips(&self) -> Option<&str> {
        Some(self.additional_tips.as_str())
    }
}