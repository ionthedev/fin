//! Shared Mermaid documents, layout, and editing for Fin's desktop and terminal interfaces.

pub mod document;
pub mod flow;
pub mod preview;
pub mod render;
#[cfg(feature = "tui")]
pub mod tui;
#[cfg(feature = "tui")]
pub mod tui_cli;

pub const EXAMPLE: &str = include_str!("../examples/welcome.mmd");
pub const EMPTY: &str = "flowchart TD\n";
