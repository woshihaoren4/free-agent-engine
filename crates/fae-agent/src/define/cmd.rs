/// 命令类型，表示系统和用户命令
#[derive(Default, Debug)]
pub enum Command {
    /// 无命令
    #[default]
    None,
    /// 系统退出命令, /exit
    SystemExit,
    /// 自定义命令
    CustomCommand(String),
}
impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Command::None, Command::None) => true,
            (Command::SystemExit, Command::SystemExit) => true,
            (Command::CustomCommand(a), Command::CustomCommand(b)) => a == b,
            _ => false,
        }
    }
}
