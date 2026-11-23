//! Host discovery settings and internal errors. Public wire schemas live in fae-agent.
mod control;
mod error;
pub use control::*;
pub use error::*;

pub const DEFAULT_PORT: u16 = 6666;
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
