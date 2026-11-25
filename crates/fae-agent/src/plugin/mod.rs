//! Plugin declarations are independent of the optional Python executor.
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

pub mod adapter;
pub mod codec;
pub mod config;
pub mod manager;
pub use manager::{PluginExecutionBinding, PluginManager, PluginSnapshot};

pub enum PluginValue {
    Json(serde_json::Value),
    Model(crate::ModelResponse),
    Tool(crate::ToolResponse),
}

#[async_trait::async_trait]
pub trait PluginExecutor: std::fmt::Debug + Send + Sync {
    fn status(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        ctx: &crate::Ctx,
        root: &crate::Ctx,
        point: &str,
        method: &str,
        location: Option<&str>,
        metadata: serde_json::Value,
        argument: PluginValue,
    ) -> anyhow::Result<PluginValue>;
}

pub const API_VERSION: &str = "0.2.0";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMode {
    Chain,
    #[default]
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginEngine {
    Py,
    Js,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "point", deny_unknown_fields)]
pub enum PluginMount {
    SingleAgentHook {
        implementation: String,
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
    },
    ToolsHook {
        implementation: String,
        #[serde(default)]
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
    },
    ModelHook {
        implementation: String,
    },
}

impl PluginMount {
    pub fn implementation(&self) -> &str {
        match self {
            Self::SingleAgentHook { implementation, .. }
            | Self::ToolsHook { implementation, .. }
            | Self::ModelHook { implementation, .. } => implementation,
        }
    }

    pub fn point(&self) -> &'static str {
        match self {
            Self::SingleAgentHook { .. } => "SingleAgentHook",
            Self::ToolsHook { .. } => "ToolsHook",
            Self::ModelHook { .. } => "ModelHook",
        }
    }

    pub fn location(&self) -> Option<&str> {
        match self {
            Self::SingleAgentHook { agent_id, .. } => agent_id.as_deref(),
            Self::ToolsHook { channel, .. } => channel.as_deref(),
            Self::ModelHook { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDeclaration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub registration_mode: RegistrationMode,
    pub plugin_version: String,
    #[serde(default = "default_api")]
    pub fae_plugin_api_version: String,
    pub description: String,
    #[serde(default)]
    pub engine: Option<PluginEngine>,
    pub mounts: Vec<PluginMount>,
}

fn default_api() -> String {
    API_VERSION.to_owned()
}

pub fn validate_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 64
            && id.as_bytes()[0].is_ascii_lowercase()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
        "invalid plugin ID: expected [a-z][a-z0-9_]{{0,63}}"
    );
    Ok(())
}

impl PluginDeclaration {
    pub fn normalize(
        mut self,
        id: &str,
        single_file_engine: Option<PluginEngine>,
    ) -> anyhow::Result<Self> {
        validate_id(id)?;
        if let Some(declared_id) = &self.id {
            validate_id(declared_id)?;
            anyhow::ensure!(
                declared_id == id,
                "plugin ID does not match configuration: {id}"
            );
        }
        semver::Version::parse(&self.plugin_version)?;
        anyhow::ensure!(
            self.fae_plugin_api_version == API_VERSION,
            "incompatible plugin API version: {}",
            self.fae_plugin_api_version
        );
        anyhow::ensure!(
            !self.description.trim().is_empty(),
            "description is required"
        );
        anyhow::ensure!(!self.mounts.is_empty(), "mounts must not be empty");
        if let Some(inferred) = single_file_engine {
            anyhow::ensure!(
                self.engine.is_none_or(|engine| engine == inferred),
                "engine does not match source suffix"
            );
            self.engine = Some(inferred);
        }
        let engine = self
            .engine
            .ok_or_else(|| anyhow::anyhow!("directory engine is required"))?;
        anyhow::ensure!(
            engine == PluginEngine::Py || self.registration_mode != RegistrationMode::Chain,
            "chain mode requires the Python engine"
        );
        for mount in &mut self.mounts {
            match mount {
                PluginMount::ToolsHook { channel, .. } => {
                    channel.get_or_insert_with(|| format!("fae_{id}"));
                }
                _ => {}
            }
            if let Some(location) = mount.location() {
                anyhow::ensure!(!location.trim().is_empty(), "empty mount location");
            }
            let entry = mount.implementation();
            let class = if single_file_engine.is_some() {
                anyhow::ensure!(
                    !entry.contains(':'),
                    "single-file entry must be a class name"
                );
                entry
            } else {
                let (file, class) = entry
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("directory entry must be file:class"))?;
                let path = Path::new(file);
                anyhow::ensure!(
                    !file.is_empty()
                        && path.components().all(|c| matches!(c, Component::Normal(_))),
                    "implementation path must stay within the plugin directory"
                );
                let suffix = match engine {
                    PluginEngine::Py => "py",
                    PluginEngine::Js => "js",
                };
                anyhow::ensure!(
                    path.extension().and_then(|s| s.to_str()) == Some(suffix),
                    "implementation suffix does not match engine"
                );
                class
            };
            anyhow::ensure!(
                !class.is_empty()
                    && (class.as_bytes()[0].is_ascii_alphabetic() || class.starts_with('_'))
                    && class
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "implementation must name an exported class"
            );
        }
        for (i, left) in self.mounts.iter().enumerate() {
            for right in &self.mounts[i + 1..] {
                if left.point() != right.point() || left.implementation() != right.implementation()
                {
                    continue;
                }
                let overlap = left.location() == right.location()
                    || (left.point() == "SingleAgentHook"
                        && (left.location().is_none() || right.location().is_none()));
                anyhow::ensure!(!overlap, "duplicate or overlapping class mount");
            }
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> serde_json::Value {
        serde_json::json!({"plugin_version":"1.2.3", "description":"Example",
            "mounts":[{"point":"SingleAgentHook","implementation":"Hook"}]})
    }

    #[test]
    fn defaults_and_strict_mount_fields() {
        let declaration: PluginDeclaration = serde_json::from_value(config()).unwrap();
        let declaration = declaration
            .normalize("example", Some(PluginEngine::Py))
            .unwrap();
        assert_eq!(declaration.registration_mode, RegistrationMode::Direct);
        assert_eq!(declaration.fae_plugin_api_version, API_VERSION);
        let mut invalid = config();
        invalid["mounts"][0]["channel"] = "wrong".into();
        assert!(serde_json::from_value::<PluginDeclaration>(invalid).is_err());
        let mut model = config();
        model["mounts"][0]["point"] = "ModelHook".into();
        let declaration = serde_json::from_value::<PluginDeclaration>(model.clone())
            .unwrap()
            .normalize("example", Some(PluginEngine::Py))
            .unwrap();
        assert!(declaration.mounts[0].location().is_none());
        assert!(
            serde_json::to_value(&declaration).unwrap()["mounts"][0]
                .get("model")
                .is_none()
        );
        model["mounts"][0]["model"] = "legacy-model".into();
        assert!(serde_json::from_value::<PluginDeclaration>(model).is_err());
    }

    #[test]
    fn declared_id_must_match_loading_id() {
        let mut raw = config();
        raw["id"] = "example".into();
        let declaration: PluginDeclaration = serde_json::from_value(raw).unwrap();
        assert!(
            declaration
                .clone()
                .normalize("example", Some(PluginEngine::Py))
                .is_ok()
        );
        assert!(
            declaration
                .normalize("other", Some(PluginEngine::Py))
                .is_err()
        );
    }

    #[test]
    fn rejects_invalid_entries_versions_and_overlap() {
        for (field, value) in [
            ("plugin_version", "bad"),
            ("fae_plugin_api_version", "0.1.0"),
            ("description", ""),
        ] {
            let mut invalid = config();
            invalid[field] = value.into();
            let declaration: PluginDeclaration = serde_json::from_value(invalid).unwrap();
            assert!(
                declaration
                    .normalize("example", Some(PluginEngine::Py))
                    .is_err()
            );
        }
        let mut invalid = config();
        invalid["engine"] = "py".into();
        invalid["mounts"][0]["implementation"] = "../outside.py:Hook".into();
        let declaration: PluginDeclaration = serde_json::from_value(invalid).unwrap();
        assert!(declaration.normalize("example", None).is_err());
        let mut invalid = config();
        invalid["mounts"].as_array_mut().unwrap().push(
            serde_json::json!({"point":"SingleAgentHook","implementation":"Hook","agent_id":"fae"}),
        );
        let declaration: PluginDeclaration = serde_json::from_value(invalid).unwrap();
        assert!(
            declaration
                .normalize("example", Some(PluginEngine::Py))
                .is_err()
        );
        assert!(validate_id("../example").is_err());
    }
}
