use fae_agent::{Select, Task, TaskExecutor, TaskResult, Thing, ThingSelect};
use wd_tools::PFErr;

pub struct SkillsExecutor {}

impl Default for SkillsExecutor {
    fn default() -> Self {
        Self {}
    }
}

#[async_trait::async_trait]
impl TaskExecutor for SkillsExecutor {
    fn desc(&self) -> String {
        "skill loader".to_string()
    }

    fn channel(&self) -> String {
        "default".to_string()
    }

    async fn execute(&self, _task: Task) -> anyhow::Result<TaskResult> {
        anyhow::anyhow!("Skill not implemented exec.").err()
    }

    async fn query(&self, select: Select) -> anyhow::Result<Vec<Thing>> {
        let (_channel, name, dir) = if let ThingSelect::Skill(channel, name, dir) = select.select {
            (channel, name, dir)
        } else {
            return anyhow::anyhow!("Skill not implemented").err();
        };
        let dir = if let Some(dir) = dir {
            dir.into()
        } else {
            fae_agent::fae_home()
                .join("skills")
                .join(name.as_str())
                .join("SKILL.md")
        };
        // check dir exists
        if !dir.exists() {
            return Err(anyhow::anyhow!(
                "[Skill:{}] not found: {}",
                name,
                dir.display()
            ));
        }
        // check dir is a file
        if !dir.is_file() {
            return Err(anyhow::anyhow!(
                "[Skill:{}] is not a file: {}",
                name,
                dir.display()
            ));
        }
        let content = tokio::fs::read_to_string(&dir).await?;

        let mut header_str = None;
        let mut body = content.as_str();

        if content.starts_with("---") {
            let mut parts = content.splitn(3, "---");
            parts.next(); // skip empty string before first ---
            if let Some(h) = parts.next() {
                if let Some(b) = parts.next() {
                    header_str = Some(h);
                    body = b;
                }
            }
        }

        let header = if let Some(h_str) = header_str {
            match serde_yaml::from_str::<fae_agent::SkillHeader>(h_str) {
                Ok(h) => h,
                Err(e) => return Err(anyhow::anyhow!("Failed to parse skill header: {}", e)),
            }
        } else {
            let mut h = fae_agent::SkillHeader::default();
            h.name = name.clone();
            h
        };

        let mut thing = Thing::new(_channel);
        thing.add_item(fae_agent::ThingItem::Skill(header));

        Ok(vec![thing])
    }
}
