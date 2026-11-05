//! Terminal presentation only; application code owns session transport.
mod file_browser;
mod input;
mod log_capture;
mod render;
mod state;
mod terminal;

pub use state::Mode;
pub use terminal::{PromptAction, TerminalUi, UiAction};
