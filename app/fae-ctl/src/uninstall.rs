use std::fs;
use std::io::{self, Write};

pub struct Uninstall{

}

impl Uninstall{
    pub fn exec(&self){
        println!("uninstall agent");
        // 生成四位随机数
        let rnd = wd_tools::rand::random_in_between::<u32, _>(1000..10000);
        
        // 让用户输入
        println!("Please type '{}' to confirm uninstallation:", rnd);
        print!("> ");
        let _ = io::stdout().flush();
        
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            println!("Failed to read input. Uninstallation aborted.");
            return;
        }

        // 随机数和输入相同
        if input.trim() == rnd.to_string() {
            // 删除 ~/.fae
            if let Some(mut home) = dirs::home_dir() {
                home.push(".fae");
                if home.exists() {
                    match fs::remove_dir_all(&home) {
                        Ok(_) => println!("Successfully removed {:?}", home),
                        Err(e) => println!("Failed to remove {:?}: {}", home, e),
                    }
                } else {
                    println!("{:?} does not exist, nothing to remove.", home);
                }
            } else {
                println!("Could not determine home directory.");
            }
        } else {
            println!("Input does not match. Uninstallation aborted.");
        }
    }
}