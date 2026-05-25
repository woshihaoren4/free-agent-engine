use thiserror::Error;

// 任务执行错误码 未知错误
pub const TASK_ERROR_CODE_UNKNOWN: i32 = 999001001;
// 计划执行报错，计划被终止， 错误来源计划本身
pub const TASK_ERROR_CODE_PLAN_ABORT: i32 = 999001002;
// 计划执行报错，计划被终止， 错误来源执行器
pub const TASK_ERROR_CODE_PLAN_ABORT_EXTERNAL: i32 = 999001003;
// 计划被用户强行终止
pub const TASK_ERROR_CODE_PLAN_ABORT_USER: i32 = 999001004;

#[derive(Error, Debug)]
pub enum Error {
    #[error("NoSupport")]
    NoSupport,

    #[error("Session error: {0}")]
    Session(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Custom error: {0}")]
    Custom(String),

    #[error("anyhow error: {0}")]
    Anyhow(#[from] anyhow::Error),
}
