//! `lunacy` — a small local CI/deploy daemon.
//!
//! The daemon loop, HTTP API and WebSocket hub live here as a library so the integration
//! tests can drive them without spawning a process. The engine they run (config, git, act,
//! pipeline, reporting) is `lunacy-core`, re-exported under the old paths.

pub use lunacy_core::{
    act, config, configfile, deploy_check, discord, exec, git, github_app, model, pipeline, report,
};

pub mod api;
pub mod daemon;
pub mod ws;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
