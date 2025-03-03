//! Rill crate.

#![warn(missing_docs)]

mod actors;
pub mod config;
mod state;
pub mod tracers;

metacrate::meta!();

pub use actors::engine::RillEngine;
pub use config::EngineConfig;
