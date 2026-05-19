use serde::{Deserialize, Serialize};

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
