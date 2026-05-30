pub struct InitProject{

}

impl InitProject{
    pub fn init(ws:String){
        wd_log::log_info_ln!("start init project...");
        // 设置环境变量FAE_WORKSPACE为～/.fae
        // 创建～/.fae/prompt目录
        // 创建～/.fae/{ws}目录
        // 在～/.fae/{ws}目录下创建single_agent
    }
}