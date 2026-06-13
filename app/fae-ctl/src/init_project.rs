use fae_agent::AgentConfigData;
use std::env;
use std::fs;
use std::path::PathBuf;

const PROMPT_AICODING: &str = include_str!("../../../docs/prompt/aicoding.txt");
const PROMPT_CLAW: &str = include_str!("../../../docs/prompt/claw.txt");
const MCP_LIST_JSON: &str = r#"{
	"mcpServers": {
		"mcp_name1": {
			"url": "https://mcp.xxxx.com/mcp",
			"headers": {}
		},
		"mcp_name2": {
			"command": "npx",
			"args": ""
		}
	}
}"#;

pub struct InitProject {}

impl InitProject {
    pub fn get_workspace_dir(ws: &str) -> PathBuf {
        let fae_dir = fae_agent::fae_home();
        fae_dir.join(ws)
    }
    pub async fn init(ws: String) {
        wd_log::log_info_ln!("start init project...");

        let fae_dir = fae_agent::fae_home();

        // 设置环境变量FAE_WORKSPACE为～/.fae
        unsafe {
            wd_log::log_info_ln!("set env FAE_WORKSPACE to {}", fae_dir.display());
            env::set_var("FAE_WORKSPACE", fae_dir.to_str().unwrap());
        }
        // 下载site.txt
        wd_log::log_info_ln!("downloading from site.txt...");
        let site_txt_url = "https://woshihaoren4.github.io/free-agent-engine/site.txt";
        match reqwest::get(site_txt_url).await {
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(site_content) = resp.text().await {
                        for line in site_content.lines() {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            let file_path = fae_dir.join(line);
                            if !file_path.exists() {
                                let url = format!(
                                    "https://woshihaoren4.github.io/free-agent-engine/{}",
                                    line
                                );
                                wd_log::log_info_ln!("downloading {} ...", url);
                                match reqwest::get(&url).await {
                                    Ok(resp) => {
                                        if resp.status().is_success() {
                                            if let Ok(content) = resp.bytes().await {
                                                if let Some(parent) = file_path.parent() {
                                                    if !parent.exists() {
                                                        if let Err(e) = fs::create_dir_all(parent) {
                                                            wd_log::log_error_ln!(
                                                                "failed to create dir {}: {}",
                                                                parent.display(),
                                                                e
                                                            );
                                                        }
                                                    }
                                                }
                                                if let Err(e) = fs::write(&file_path, content) {
                                                    wd_log::log_error_ln!(
                                                        "failed to write file {}: {}",
                                                        file_path.display(),
                                                        e
                                                    );
                                                } else {
                                                    wd_log::log_info_ln!(
                                                        "downloaded and saved {}",
                                                        file_path.display()
                                                    );
                                                    if file_path
                                                        .extension()
                                                        .and_then(|s| s.to_str())
                                                        == Some("zip")
                                                    {
                                                        wd_log::log_info_ln!(
                                                            "extracting zip file {}...",
                                                            file_path.display()
                                                        );
                                                        match fs::File::open(&file_path) {
                                                            Ok(file) => {
                                                                match zip::ZipArchive::new(file) {
                                                                    Ok(mut archive) => {
                                                                        let target_dir = file_path
                                                                            .parent()
                                                                            .unwrap();
                                                                        if let Err(e) = archive
                                                                            .extract(target_dir)
                                                                        {
                                                                            wd_log::log_error_ln!(
                                                                                "failed to extract zip file {}: {}",
                                                                                file_path.display(),
                                                                                e
                                                                            );
                                                                        } else {
                                                                            wd_log::log_info_ln!(
                                                                                "extracted zip file {}",
                                                                                file_path.display()
                                                                            );
                                                                            if let Err(e) =
                                                                                fs::remove_file(
                                                                                    &file_path,
                                                                                )
                                                                            {
                                                                                wd_log::log_error_ln!(
                                                                                    "failed to remove zip file {}: {}",
                                                                                    file_path
                                                                                        .display(),
                                                                                    e
                                                                                );
                                                                            } else {
                                                                                wd_log::log_info_ln!(
                                                                                    "removed zip file {}",
                                                                                    file_path
                                                                                        .display()
                                                                                );
                                                                            }
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        wd_log::log_error_ln!(
                                                                            "failed to read zip archive {}: {}",
                                                                            file_path.display(),
                                                                            e
                                                                        )
                                                                    }
                                                                }
                                                            }
                                                            Err(e) => wd_log::log_error_ln!(
                                                                "failed to open zip file {}: {}",
                                                                file_path.display(),
                                                                e
                                                            ),
                                                        }
                                                    }
                                                }
                                            }
                                        } else {
                                            wd_log::log_error_ln!(
                                                "failed to download {}: status {}",
                                                url,
                                                resp.status()
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        wd_log::log_error_ln!("failed to download {}: {}", url, e);
                                    }
                                }
                            } else {
                                wd_log::log_info_ln!(
                                    "file {} already exists, skip",
                                    file_path.display()
                                );
                            }
                        }
                    }
                } else {
                    wd_log::log_error_ln!("failed to fetch site.txt: status {}", resp.status());
                }
            }
            Err(e) => {
                wd_log::log_error_ln!("failed to fetch site.txt: {}", e);
            }
        }
        // 创建～/.fae/prompt目录
        let prompt_dir = fae_dir.join("prompt");
        if !prompt_dir.exists() {
            wd_log::log_info_ln!("create directory {}", prompt_dir.display());
            fs::create_dir_all(&prompt_dir).expect("Failed to create prompt directory");
        } else {
            wd_log::log_info_ln!("directory {} already exists", prompt_dir.display());
        }

        // 检查并创建 ～/.fae/mcp/mcp_list.json文件,写入：MCP_LIST_JSON
        let mcp_dir = fae_dir.join("mcp");
        if !mcp_dir.exists() {
            wd_log::log_info_ln!("create directory {}", mcp_dir.display());
            fs::create_dir_all(&mcp_dir).expect("Failed to create mcp directory");
        } else {
            wd_log::log_info_ln!("directory {} already exists", mcp_dir.display());
        }

        let mcp_list_path = mcp_dir.join("mcp_list.json");
        if !mcp_list_path.exists() {
            wd_log::log_info_ln!("create file {}", mcp_list_path.display());
            fs::write(&mcp_list_path, MCP_LIST_JSON).expect("Failed to write mcp_list.json");
        } else {
            wd_log::log_info_ln!("file {} already exists", mcp_list_path.display());
        }

        // 检查并在～/.fae/prompt目录下创建aicoding.txt和claw.txt文件
        let aicoding_path = prompt_dir.join("aicoding.txt");
        if !aicoding_path.exists() {
            wd_log::log_info_ln!("create file {}", aicoding_path.display());
            fs::write(&aicoding_path, PROMPT_AICODING).expect("Failed to write aicoding.txt");
        } else {
            wd_log::log_info_ln!("file {} already exists", aicoding_path.display());
        }

        let claw_path = prompt_dir.join("claw.txt");
        if !claw_path.exists() {
            wd_log::log_info_ln!("create file {}", claw_path.display());
            fs::write(&claw_path, PROMPT_CLAW).expect("Failed to write claw.txt");
        } else {
            wd_log::log_info_ln!("file {} already exists", claw_path.display());
        }

        // 创建～/.fae/{ws}目录
        let ws_dir = fae_dir.join(&ws);
        if !ws_dir.exists() {
            wd_log::log_info_ln!("create directory {}", ws_dir.display());
            fs::create_dir_all(&ws_dir).expect("Failed to create workspace directory");
        } else {
            wd_log::log_info_ln!("directory {} already exists", ws_dir.display());
        }

        // 检查并在～/.fae/{ws}目录下创建main agent prompt：claw.txt
        let main_agent_dir = ws_dir.join("main");
        if !main_agent_dir.exists() {
            wd_log::log_info_ln!("create agent main in {}", ws_dir.display());
            fs::create_dir_all(&main_agent_dir).expect("Failed to create main agent directory");
            let mut main_config = AgentConfigData::default()
                .set_name("风筝小管家")
                .set_description("风筝小管家是一个智能助手，用于回复主人的任何问题，并提供一定的执行能力，并且会记得主人的任何嘱托。")
                .set_prompt_path(format!("{}/prompt/claw.txt", fae_dir.display()));
            main_config
                .init("main", &ws_dir)
                .await
                .expect("Failed to init main agent config");
        } else {
            wd_log::log_info_ln!("agent main already exists in {}", ws_dir.display());
        }

        // 检查并在～/.fae/{ws}目录下创建fae_coding，prompt：aicoding.txt
        let fae_coding_agent_dir = ws_dir.join("fae_coding");
        if !fae_coding_agent_dir.exists() {
            wd_log::log_info_ln!("create agent fae_coding in {}", ws_dir.display());
            fs::create_dir_all(&fae_coding_agent_dir)
                .expect("Failed to create fae_coding agent directory");
            let mut fae_coding_config = AgentConfigData::default()
                .set_name("风筝编程助手")
                .set_description(
                    "风筝编程助手是一个智能编程助手，用于项目开发，写脚本，错误修复，代码优化等。",
                )
                .set_prompt_path(format!("{}/prompt/aicoding.txt", fae_dir.display()));
            fae_coding_config
                .init("fae_coding", &ws_dir)
                .await
                .expect("Failed to init fae_coding agent config");
        } else {
            wd_log::log_info_ln!("agent fae_coding already exists in {}", ws_dir.display());
        }

        wd_log::log_info_ln!("init project success.");
    }
}
