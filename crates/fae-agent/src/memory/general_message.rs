use serde::{Deserialize, Serialize};
use wd_tools;

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeneralMessageType {
    System,
    #[default]
    Query,
    Answer,
    Tool,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralMessage {
    pub id: String,
    pub r#type: GeneralMessageType,
    pub payload: String,
}

impl crate::memory::MemoryRuler for GeneralMessage {
    fn as_content(&self) -> String {
        self.payload.clone()
    }

    fn from_content(content: String) -> Self {
        Self {
            id: wd_tools::uuid::v4(),
            r#type: GeneralMessageType::Answer,
            payload: content,
        }
    }
}
