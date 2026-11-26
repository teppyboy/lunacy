//! One module per screen.

pub mod deploy;
pub mod diagnostics;
pub mod project;
pub mod projects;
pub mod run;
pub mod settings;
pub mod steps;

/// The GitHub repo a project deploys from: the live list, or demo fixtures only before live data
/// has answered.
pub fn repo_for(s: &crate::state::State, project: &str) -> Option<String> {
    let from_project = s
        .projects
        .iter()
        .find(|p| p.name == project)
        .and_then(|p| p.github_repo.clone())
        .filter(|r| !r.is_empty());
    if s.loaded {
        from_project
    } else {
        from_project.or_else(|| crate::demo::repo_for(project))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Project;
    use crate::state::{Conn, State};

    #[test]
    fn live_projects_do_not_fall_back_to_demo_github_repo() {
        let mut s = State::new(Conn::default());
        s.loaded = false;
        assert_eq!(
            repo_for(&s, "lunarum").as_deref(),
            crate::demo::repo_for("lunarum").as_deref()
        );
        s.loaded = true;
        s.projects.push(Project {
            name: "lunarum".into(),
            github_repo: None,
            ..Project::default()
        });
        assert_eq!(repo_for(&s, "lunarum"), None);
    }
}
