//! `lunacy` — a small local CI/deploy daemon.
//!
//! The daemon, pipeline and HTTP API live here as a library so the integration
//! tests can drive the pipeline and the API without spawning a process.

pub mod act;
pub mod api;
pub mod config;
pub mod configfile;
pub mod daemon;
pub mod discord;
pub mod exec;
pub mod git;
pub mod github_app;
pub mod model;
pub mod pipeline;
pub mod report;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
