//! One module per screen.

pub mod diagnostics;
pub mod project;
pub mod projects;
pub mod run;
pub mod settings;
pub mod steps;

/// The GitHub repo a project deploys from: the live list, else the demo fixtures when the window
/// is showing fixtures.
pub fn repo_for(s: &crate::state::State, project: &str) -> Option<String> {
    s.projects
        .iter()
        .find(|p| p.name == project)
        .and_then(|p| p.github_repo.clone())
        .filter(|r| !r.is_empty())
        .or_else(|| crate::demo::repo_for(project))
}
