//! Installation order and lifecycle metadata, independent of runtime workers.
use super::validate_id;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledPlugin {
    pub id: String,
    #[serde(default)]
    pub source: String,
    /// Directory relative to <fae_home>/plugins; empty keeps ID-based lookup.
    #[serde(default)]
    pub directory_path: String,
    pub installed: bool,
    pub enabled: bool,
}

impl InstalledPlugin {
    pub fn source_path(&self, home: &Path) -> PathBuf {
        let root = home.join("plugins");
        if !self.directory_path.is_empty() {
            return root.join(&self.directory_path);
        }
        [
            root.join(format!("{}.py", self.id)),
            root.join(&self.id),
            root.join(format!("{}.js", self.id)),
        ]
        .into_iter()
        .find(|path| path.exists())
        .unwrap_or_else(|| root.join(format!("{}.py", self.id)))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginsConfig {
    pub plugins: Vec<InstalledPlugin>,
}

impl PluginsConfig {
    pub fn read(home: &Path) -> anyhow::Result<Self> {
        let path = home.join("plugins/plugins.config");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let config: Self =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        let mut ids = std::collections::HashSet::new();
        for plugin in &config.plugins {
            validate_id(&plugin.id)?;
            anyhow::ensure!(ids.insert(&plugin.id), "duplicate plugin ID: {}", plugin.id);
            if !plugin.directory_path.is_empty() {
                anyhow::ensure!(
                    Path::new(&plugin.directory_path)
                        .components()
                        .all(|component| matches!(component, Component::Normal(_))),
                    "plugin directory_path must be relative and stay within plugins: {}",
                    plugin.directory_path
                );
            }
        }
        Ok(config)
    }

    pub fn write(&self, home: &Path) -> anyhow::Result<()> {
        let root = home.join("plugins");
        std::fs::create_dir_all(&root)?;
        let path = root.join("plugins.config");
        let temp = root.join(format!(".plugins-{}.tmp", wd_tools::uuid::v4()));
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        let result = std::fs::rename(&temp, &path);
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result.with_context(|| format!("write {}", path.display()))
    }
}

pub fn install(home: &Path, source: &Path) -> anyhow::Result<String> {
    let source = source
        .canonicalize()
        .with_context(|| format!("plugin source {}", source.display()))?;
    let filename = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("plugin source must have a UTF-8 filename"))?;
    let is_directory = source.is_dir();
    let id = if is_directory {
        #[derive(Deserialize)]
        struct DirectoryIdentity {
            id: String,
        }
        let path = source.join("plugin.config");
        let identity: DirectoryIdentity = serde_json::from_slice(&std::fs::read(&path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        identity.id
    } else {
        anyhow::ensure!(
            matches!(
                source.extension().and_then(|s| s.to_str()),
                Some("py" | "js")
            ),
            "single-file plugin must have a .py or .js suffix"
        );
        source.file_stem().unwrap().to_str().unwrap().to_owned()
    };
    validate_id(&id)?;
    let root = home.join("plugins");
    std::fs::create_dir_all(&root)?;
    anyhow::ensure!(
        !source.is_dir() || !root.canonicalize()?.starts_with(&source),
        "plugin source must not contain the plugins installation directory"
    );
    let _lock = lock(&root)?;
    let mut config = PluginsConfig::read(home)?;
    let destination = root.join(filename);
    anyhow::ensure!(
        !matches!(filename, "plugins.config" | ".plugins.lock")
            && !filename.starts_with(".install-")
            && !filename.starts_with(".plugins-"),
        "reserved plugin installation filename: {filename}"
    );
    let mut replaced = vec![destination.clone()];
    if let Some(previous) = config.plugins.iter().find(|plugin| plugin.id == id) {
        replaced.push(previous.source_path(home));
    }
    for plugin in config.plugins.iter().filter(|plugin| plugin.id != id) {
        let path = plugin.source_path(home);
        anyhow::ensure!(
            !replaced
                .iter()
                .any(|target| path.starts_with(target) || target.starts_with(&path)),
            "plugin destination conflicts with plugin {}",
            plugin.id
        );
    }
    let staging = root.join(format!(".install-{}", wd_tools::uuid::v4()));
    // Stage first so installing from the destination itself also works.
    if let Err(error) = copy(&source, &staging) {
        let _ = remove(&staging);
        return Err(error);
    }
    for path in replaced {
        if let Err(error) = remove(&path) {
            let _ = remove(&staging);
            return Err(error);
        }
    }
    std::fs::rename(&staging, destination)?;
    let record = InstalledPlugin {
        id: id.clone(),
        source: filename.into(),
        directory_path: if is_directory {
            filename.into()
        } else {
            String::new()
        },
        installed: true,
        enabled: true,
    };
    if let Some(index) = config.plugins.iter().position(|plugin| plugin.id == id) {
        if config.plugins[index].installed {
            config.plugins[index] = record;
        } else {
            config.plugins.remove(index);
            config.plugins.push(record);
        }
    } else {
        config.plugins.push(record);
    }
    config.write(home)?;
    Ok(id)
}

pub fn uninstall(home: &Path, id: &str) -> anyhow::Result<()> {
    validate_id(id)?;
    let root = home.join("plugins");
    std::fs::create_dir_all(&root)?;
    let _lock = lock(&root)?;
    let mut config = PluginsConfig::read(home)?;
    let plugin = config
        .plugins
        .iter_mut()
        .find(|plugin| plugin.id == id && plugin.installed)
        .ok_or_else(|| anyhow::anyhow!("plugin is not installed: {id}"))?;
    plugin.installed = false;
    plugin.enabled = false;
    config.write(home)
}

fn lock(root: &Path) -> anyhow::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".plugins.lock"))?;
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}

fn remove(path: &Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn copy(source: &Path, target: &Path) -> anyhow::Result<()> {
    if source.is_dir() {
        std::fs::create_dir(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            anyhow::ensure!(
                !entry.file_type()?.is_symlink(),
                "plugin source symlinks are not supported"
            );
            copy(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(source, target)?;
    }
    Ok(())
}
