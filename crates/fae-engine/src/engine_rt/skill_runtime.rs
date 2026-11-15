use std::path::{Component, Path, PathBuf};

use fae_agent::{RuntimeSelectExec, SkillInfo, SkillQuery, TaskType};
use serde::Deserialize;
use serde_json::Value;

use super::default_fae_host;

const SKILL_FILE: &str = "SKILL.md";

#[derive(Debug, Clone)]
pub struct SkillRuntime {
    host_dir: PathBuf,
    agents_skills_dir: Option<PathBuf>,
}

impl Default for SkillRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillRuntime {
    pub const ID: &'static str = "skill_default";

    pub fn new() -> Self {
        Self::with_host_dir(default_fae_host())
    }

    pub fn with_host_dir(host_dir: impl Into<PathBuf>) -> Self {
        Self {
            host_dir: host_dir.into(),
            agents_skills_dir: std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .or_else(|| std::env::var_os("USERPROFILE").filter(|home| !home.is_empty()))
                .map(|home| PathBuf::from(home).join(".agents/skills")),
        }
    }

    pub fn host_dir(&self) -> &Path {
        &self.host_dir
    }

    pub async fn query(&self, query: SkillQuery) -> fae_agent::Result<Vec<SkillInfo>> {
        let path = match query {
            SkillQuery::Name(name) => {
                validate_skill_name(&name)?;
                let path = self.host_dir.join("skills").join(&name).join(SKILL_FILE);
                if !tokio::fs::try_exists(&path)
                    .await
                    .map_err(|error| anyhow::anyhow!("skill path `{}`: {error}", path.display()))?
                    && let Some(agents_skills_dir) = &self.agents_skills_dir
                {
                    agents_skills_dir.join(name).join(SKILL_FILE)
                } else {
                    path
                }
            }
            SkillQuery::Path(path) => path,
        };

        Ok(discover_skills(&path).await?)
    }
}

#[async_trait::async_trait]
impl RuntimeSelectExec<(), (), SkillQuery, Vec<SkillInfo>> for SkillRuntime {
    fn id(&self) -> &str {
        Self::ID
    }

    fn tys(&self) -> Vec<TaskType> {
        vec![TaskType::Skill]
    }

    async fn select(&self, ty: TaskType, query: SkillQuery) -> fae_agent::Result<Vec<SkillInfo>> {
        if ty != TaskType::Skill {
            return Err(fae_agent::Error::RuntimeNoSupport);
        }
        self.query(query).await
    }
}

#[derive(Debug, Default, Deserialize)]
struct SkillHeader {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
}

async fn discover_skills(path: &Path) -> anyhow::Result<Vec<SkillInfo>> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| anyhow::anyhow!("skill path `{}`: {error}", path.display()))?;
    if metadata.is_file() {
        anyhow::ensure!(
            path.file_name().and_then(|name| name.to_str()) == Some(SKILL_FILE),
            "skill file must be named {SKILL_FILE}: {}",
            path.display()
        );
        return Ok(vec![load_skill(path).await?]);
    }
    anyhow::ensure!(metadata.is_dir(), "skill path is not a directory");

    let mut pending = vec![path.to_path_buf()];
    let mut skill_paths = Vec::new();
    while let Some(dir) = pending.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let entry_path = entry.path();
            if file_type.is_dir() {
                pending.push(entry_path);
            } else if file_type.is_file()
                && entry_path.file_name().and_then(|name| name.to_str()) == Some(SKILL_FILE)
            {
                skill_paths.push(entry_path);
            }
        }
    }

    skill_paths.sort();
    let mut skills = Vec::with_capacity(skill_paths.len());
    for skill_path in skill_paths {
        skills.push(load_skill(&skill_path).await?);
    }
    Ok(skills)
}

async fn load_skill(path: &Path) -> anyhow::Result<SkillInfo> {
    let content = tokio::fs::read_to_string(path).await?;
    let header = parse_header(&content).map_err(|error| {
        anyhow::anyhow!("invalid skill metadata in `{}`: {error}", path.display())
    })?;
    let fallback_name = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    Ok(SkillInfo {
        name: if header.name.trim().is_empty() {
            fallback_name.to_string()
        } else {
            header.name
        },
        description: header.description,
        path: path.to_path_buf(),
        version: header.version,
        metadata: header.metadata,
    })
}

fn parse_header(content: &str) -> anyhow::Result<SkillHeader> {
    let Some(rest) = content.strip_prefix("---") else {
        return Ok(SkillHeader::default());
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'));
    let Some(rest) = rest else {
        return Ok(SkillHeader::default());
    };
    let Some((header, _)) = rest.split_once("\n---") else {
        anyhow::bail!("unterminated YAML front matter");
    };
    Ok(serde_yaml::from_str(header)?)
}

fn validate_skill_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!name.is_empty(), "skill name must not be empty");
    let path = Path::new(name);
    anyhow::ensure!(
        path.components().count() == 1
            && matches!(path.components().next(), Some(Component::Normal(_))),
        "skill name must be a single path segment"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queries_skill_by_name_and_directory() -> anyhow::Result<()> {
        let host = std::env::temp_dir().join(format!(
            "fae-skill-runtime-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ));
        let alpha = host.join("skills/alpha");
        let nested = host.join("custom/nested/beta");
        tokio::fs::create_dir_all(&alpha).await?;
        tokio::fs::create_dir_all(&nested).await?;
        tokio::fs::write(
            alpha.join(SKILL_FILE),
            "---\nname: alpha\ndescription: Alpha skill\nversion: 1.0.0\n---\n# Alpha\n",
        )
        .await?;
        tokio::fs::write(
            nested.join(SKILL_FILE),
            "---\nname: beta\ndescription: Beta skill\n---\n# Beta\n",
        )
        .await?;

        let runtime = SkillRuntime::with_host_dir(&host);
        let named = runtime.query(SkillQuery::Name("alpha".into())).await?;
        assert_eq!(named[0].name, "alpha");
        assert_eq!(named[0].description, "Alpha skill");
        assert_eq!(named[0].path, alpha.join(SKILL_FILE));

        let directory = runtime.query(SkillQuery::Path(host.join("custom"))).await?;
        assert_eq!(directory.len(), 1);
        assert_eq!(directory[0].name, "beta");

        tokio::fs::remove_dir_all(host).await?;
        Ok(())
    }

    #[tokio::test]
    async fn queries_agents_skill_and_prefers_host_skill() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let host = temp.path().join("fae");
        let agents_skills_dir = temp.path().join(".agents/skills");
        let agents_skill = agents_skills_dir.join("alpha").join(SKILL_FILE);
        let host_skill = host.join("skills/alpha").join(SKILL_FILE);
        tokio::fs::create_dir_all(agents_skill.parent().unwrap()).await?;
        tokio::fs::write(&agents_skill, "---\ndescription: Agents skill\n---\n").await?;

        let runtime = SkillRuntime {
            host_dir: host,
            agents_skills_dir: Some(agents_skills_dir),
        };
        let skills = runtime.query(SkillQuery::Name("alpha".into())).await?;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].path, agents_skill);
        assert_eq!(skills[0].description, "Agents skill");
        assert!(
            runtime
                .query(SkillQuery::Name("missing".into()))
                .await
                .is_err()
        );

        tokio::fs::create_dir_all(host_skill.parent().unwrap()).await?;
        tokio::fs::write(&host_skill, "---\ndescription: Host skill\n---\n").await?;
        let skills = runtime.query(SkillQuery::Name("alpha".into())).await?;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].path, host_skill);
        assert_eq!(skills[0].description, "Host skill");

        tokio::fs::write(&host_skill, "---\ninvalid: [\n---\n").await?;
        assert!(
            runtime
                .query(SkillQuery::Name("alpha".into()))
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejects_traversal_in_skill_name() {
        let runtime = SkillRuntime::with_host_dir("unused");
        assert!(
            runtime
                .query(SkillQuery::Name("../secret".into()))
                .await
                .is_err()
        );
    }
}
