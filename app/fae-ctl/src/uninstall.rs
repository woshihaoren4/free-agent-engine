use std::fs;
use std::io::{self, Write};

pub struct Uninstall {}

impl Uninstall {
    pub fn exec(&self, ws: String) {
        println!("uninstall workspace '{}'", ws);
        // 生成四位随机数
        let rnd = wd_tools::rand::random_in_between::<u32, _>(1000..10000);

        // 让用户输入
        println!(
            "Please type '{}' to confirm uninstallation of workspace '{}':",
            rnd, ws
        );
        print!("> ");
        let _ = io::stdout().flush();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            println!("Failed to read input. Uninstallation aborted.");
            return;
        }

        // 随机数和输入相同
        if input.trim() == rnd.to_string() {
            // 只删除当前 workspace，保留 ~/.fae 下的 skills、mcp、prompt 等目录
            let workspace_dir = fae_agent::fae_home().join(&ws);
            if workspace_dir.exists() {
                match fs::remove_dir_all(&workspace_dir) {
                    Ok(_) => println!("Successfully removed {:?}", workspace_dir),
                    Err(e) => println!("Failed to remove {:?}: {}", workspace_dir, e),
                }
            } else {
                println!("{:?} does not exist, nothing to remove.", workspace_dir);
            }
        } else {
            println!("Input does not match. Uninstallation aborted.");
        }
    }
}
