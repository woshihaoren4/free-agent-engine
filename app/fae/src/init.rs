use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

use anyhow::Context;
use fae_agent::{SingleAgentConfig, SingleAgentInfo, SingleAgentModelConfig, SkillQuery};
use fae_engine::DEFAULT_TOOL_NAMES;
use tokio::io::AsyncWriteExt;

use crate::args::InitArgs;

pub struct InitResult {
    pub config_path: PathBuf,
    pub skill_count: usize,
}

pub async fn initialize(home: &Path, args: &InitArgs) -> anyhow::Result<InitResult> {
    validate_agent_id(&args.agent_id)?;
    anyhow::ensure!(!args.model.trim().is_empty(), "model must not be empty");

    let agents_dir = home.join("agents");
    let skills_dir = home.join("skills");
    let config_path = agents_dir.join(format!("{}_config.json", args.agent_id));

    if !args.force {
        anyhow::ensure!(
            !config_path.exists(),
            "refusing to overwrite `{}`; use --force to replace the agent config",
            config_path.display()
        );
    }

    tokio::fs::create_dir_all(&agents_dir)
        .await
        .with_context(|| format!("create `{}`", agents_dir.display()))?;
    tokio::fs::create_dir_all(&skills_dir)
        .await
        .with_context(|| format!("create `{}`", skills_dir.display()))?;
    tokio::fs::create_dir_all(home.join("workflows"))
        .await
        .with_context(|| format!("create `{}`", home.join("workflows").display()))?;
    tokio::fs::create_dir_all(home.join("mcp"))
        .await
        .with_context(|| format!("create `{}`", home.join("mcp").display()))?;

    let skills = discover_installed_skills(&skills_dir).await?;
    let config = SingleAgentConfig {
        agent: SingleAgentInfo {
            name: args.agent_id.clone(),
            user_id: "local".to_string(),
            session_id: "default".to_string(),
            metadata: HashMap::new(),
        },
        model: SingleAgentModelConfig {
            model: args.model.clone(),
            context_size: 32_000,
            history_turns: 20,
            max_completion_tokens: Some(65_536),
            temperature: None,
            max_tool_iterations: 8,
        },
        tools: DEFAULT_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        skills: skills.iter().cloned().map(SkillQuery::Name).collect(),
        mcp_servers: Vec::new(),
    };
    let mut config_bytes = serde_json::to_vec_pretty(&config)?;
    config_bytes.push(b'\n');

    if args.force {
        tokio::fs::write(&config_path, config_bytes)
            .await
            .with_context(|| format!("write `{}`", config_path.display()))?;
    } else {
        write_new(&config_path, &config_bytes).await?;
    }

    Ok(InitResult {
        config_path,
        skill_count: skills.len(),
    })
}

async fn discover_installed_skills(skills_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut entries = tokio::fs::read_dir(skills_dir)
        .await
        .with_context(|| format!("read `{}`", skills_dir.display()))?;
    let mut skills = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir()
            && tokio::fs::try_exists(entry.path().join("SKILL.md")).await?
        {
            let name = entry.file_name().into_string().map_err(|name| {
                anyhow::anyhow!("skill directory name is not valid UTF-8: {name:?}")
            })?;
            skills.push(name);
        }
    }
    skills.sort();
    Ok(skills)
}

async fn write_new(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .with_context(|| format!("create `{}`", path.display()))?;
    file.write_all(content)
        .await
        .with_context(|| format!("write `{}`", path.display()))
}

fn validate_agent_id(agent_id: &str) -> anyhow::Result<()> {
    let mut components = Path::new(agent_id).components();
    anyhow::ensure!(
        !agent_id.is_empty()
            && matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none(),
        "agent id must be a single non-empty path component"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initializes_agent_with_all_tools_and_installed_skills() {
        let home = std::env::temp_dir().join(format!(
            "fae-cli-init-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ));
        for skill in ["zeta", "alpha"] {
            let dir = home.join("skills").join(skill);
            tokio::fs::create_dir_all(&dir).await.unwrap();
            tokio::fs::write(dir.join("SKILL.md"), format!("# {skill}"))
                .await
                .unwrap();
        }

        let result = initialize(
            &home,
            &InitArgs {
                agent_id: "fae".to_string(),
                model: "test-model".to_string(),
                force: false,
            },
        )
        .await
        .unwrap();
        let config: SingleAgentConfig =
            serde_json::from_slice(&tokio::fs::read(&result.config_path).await.unwrap()).unwrap();

        assert_eq!(config.agent.name, "fae");
        assert_eq!(config.model.model, "test-model");
        assert_eq!(config.model.max_completion_tokens, Some(65_536));
        assert_eq!(
            config.tools,
            DEFAULT_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            config.skills,
            vec![
                SkillQuery::Name("alpha".into()),
                SkillQuery::Name("zeta".into())
            ]
        );
        assert_eq!(result.skill_count, 2);
        assert!(!home.join("agents/fae_prompt.txt").exists());

        tokio::fs::remove_dir_all(home).await.unwrap();
    }

    #[tokio::test]
    async fn refuses_to_overwrite_existing_config_without_touching_prompt() {
        let home = std::env::temp_dir().join(format!(
            "fae-cli-init-conflict-{}-{}",
            std::process::id(),
            wd_tools::uuid::v4()
        ));
        let agents = home.join("agents");
        tokio::fs::create_dir_all(&agents).await.unwrap();
        tokio::fs::write(agents.join("fae_config.json"), "existing")
            .await
            .unwrap();
        tokio::fs::write(agents.join("fae_prompt.txt"), "custom prompt")
            .await
            .unwrap();

        let error = initialize(
            &home,
            &InitArgs {
                agent_id: "fae".to_string(),
                model: "test-model".to_string(),
                force: false,
            },
        )
        .await
        .err()
        .unwrap();

        assert!(error.to_string().contains("--force"));
        assert_eq!(
            tokio::fs::read_to_string(agents.join("fae_prompt.txt"))
                .await
                .unwrap(),
            "custom prompt"
        );

        initialize(
            &home,
            &InitArgs {
                agent_id: "fae".to_string(),
                model: "replacement-model".to_string(),
                force: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read_to_string(agents.join("fae_prompt.txt"))
                .await
                .unwrap(),
            "custom prompt"
        );
        tokio::fs::remove_dir_all(home).await.unwrap();
    }
}
