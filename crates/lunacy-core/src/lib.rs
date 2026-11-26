//! `lunacy-core`: everything the daemon runs that is not its HTTP server or worker loop:
//! config, git, act, the pipeline, and GitHub/Discord reporting.

pub mod act;
pub mod config;
pub mod configfile;
pub mod deploy_check;
pub mod discord;
pub mod exec;
pub mod git;
pub mod github_app;
pub mod model;
pub mod pipeline;
pub mod report;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `config.example.toml` at the workspace root, for tests in any crate.
#[doc(hidden)]
pub fn example_config_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.toml")
}
