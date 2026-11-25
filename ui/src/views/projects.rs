//! The projects list: one row per watched repository, newest state at a glance.

use rune::{col, el, row, text, Cx, El, ScrollSpec, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::model::{Project, ProjectState, Run};
use crate::state::State;
use crate::theme;
use crate::widgets::{self, ago, duration, mono_chip, short_sha};

pub fn view(s: &State, k: &Kit, _cx: &mut Cx<State>, hair: f32) -> El {
    let mut list = col().w_full().gap(8.0).pb(24.0);
    for project in &s.projects {
        list = list.child(card_row(s, k, project));
    }

    let body: El = if !s.loaded && s.projects.is_empty() {
        match &s.link {
            crate::state::Link::Connecting => widgets::empty_state(k, "Connecting…", "Reaching the daemon."),
            crate::state::Link::Down { .. } | crate::state::Link::Unauthorized => {
                widgets::empty_state(k, "No projects to show", "The daemon has not answered yet.")
            }
            crate::state::Link::Ok => widgets::empty_state(
                k,
                "No projects yet",
                "Add one in Settings › Projects: a repository path, a branch, and the steps to run.",
            ),
        }
    } else if s.projects.is_empty() {
        widgets::empty_state(k, "No projects yet", "Add one in Settings › Projects.")
    } else {
        col().w_full().child(list)
    };

    col()
        .key("projects")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header(s, k, hair))
        .child(
            col()
                .key("projects-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .px(22.0)
                .pt(16.0)
                .scroll(ScrollSpec::default())
                .child(body),
        )
}

fn header(s: &State, k: &Kit, hair: f32) -> El {
    let running = s
        .projects
        .iter()
        .filter(|p| p.state() == ProjectState::Running)
        .count();
    let counts = if s.projects.is_empty() {
        if s.projects_error.is_some() {
            "daemon unreachable".to_string()
        } else {
            "loading".to_string()
        }
    } else if running > 0 {
        format!("{} projects · {running} running", s.projects.len())
    } else {
        format!("{} projects", s.projects.len())
    };
    row()
        .key("projects-header")
        .w_full()
        .px(22.0)
        .py(14.0)
        .gap(12.0)
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
        .child(
            col()
                .gap(1.0)
                .child(text("Projects").font(k.ty.ui_medium).color(k.c.text_base))
                .child(text(counts).font(k.ty.small).color(k.c.text_muted)),
        )
        .child(el().grow(1.0))
        .child(
            rune_kit::Switch::new(s.force)
                .label("Force")
                .hide_label()
                .on_toggle_tx(|s: &mut State, tx: &mut Tx| {
                    s.force = !s.force;
                    tx.instant();
                })
                .build(k)
                .key("projects-force"),
        )
        .child(
            text("Force triggers")
                .font(k.ty.small)
                .color(k.c.text_muted),
        )
        .child(
            Button::new("Settings")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    s.open_settings(crate::state::SettingsPage::Projects, tx)
                })
                .build(k)
                .key("projects-settings"),
        )
}

/// One project: name, remote, its latest run and what it is doing now.
fn card_row(s: &State, k: &Kit, project: &Project) -> El {
    let state = project.state();
    let ink = theme::status(k).for_project(state);
    let run = project.current_run.as_ref().or(project.last_run.as_ref());
    let name = project.name.clone();
    let triggering = s.triggering.as_deref() == Some(project.name.as_str());
    let directly = project.github_repo.clone();

    let mut facts = row().w_full().gap(14.0).center_y();
    if let Some(run) = run {
        facts = facts.child(widgets::run_pill(k, run.status)).child(
            row()
                .gap(6.0)
                .center_y()
                .child(mono_chip(k, &short_sha(&run.sha)))
                .child(
                    text(if run.subject.is_empty() {
                        "no commit subject".to_string()
                    } else {
                        run.subject.clone()
                    })
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .nowrap()
                    .truncate()
                    .grow(1.0),
                ),
        );
    } else {
        facts = facts.child(widgets::pill(k, "Never run", k.c.text_faint));
    }

    let mut right = row().gap(8.0).center_y();
    if project.pending_update() {
        right = right.child(widgets::pill(k, "Update waiting", theme::status(k).running));
    }
    if let Some(run) = run {
        right = right.child(
            text(timing(k, run))
                .font(k.ty.small)
                .color(k.c.text_faint)
                .nowrap(),
        );
    }
    right = right.child(
        Button::new(if triggering { "Starting…" } else { "Run now" })
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .disabled(triggering || state == ProjectState::Running)
            .help(if state == ProjectState::Running {
                "Already running"
            } else {
                "Queue a run for the current upstream commit"
            })
            .on_press_tx(move |s: &mut State, _tx: &mut Tx| s.trigger(&name))
            .build(k)
            .key(format!("run-now-{}", project.name)),
    );

    let mut card = col()
        .key(format!("project-{}", project.name))
        .w_full()
        .padding(14.0)
        .gap(8.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_01)
        .border(hair_width(k), k.c.border_muted)
        .hover(k.c.layer_02)
        .on_press_tx({
            let name = project.name.clone();
            move |s: &mut State, tx: &mut Tx| s.open_project(&name, tx)
        })
        .cursor(rune::Cursor::Pointer)
        .child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(widgets::dot(ink, 8.0))
                .child(
                    text(project.name.clone())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base),
                )
                .child(
                    text(project.branch.clone())
                        .font(k.ty.small)
                        .color(k.c.text_faint),
                )
                .child(el().grow(1.0))
                .children(directly.map(|repo| {
                    text(repo)
                        .font(k.ty.small)
                        .color(k.c.text_faint)
                        .nowrap()
                        .truncate()
                }))
                .child(widgets::project_pill(k, state)),
        )
        .child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(facts.grow(1.0))
                .child(right),
        );
    card = card.child(
        row().w_full().gap(8.0).center_y().child(
            text(format!(
                "{} · {}",
                project.repo_facts(),
                project.checkout_facts()
            ))
            .font(k.ty.small)
            .color(k.c.text_faint)
            .nowrap()
            .truncate(),
        ),
    );
    if let Some(err) = &project.last_error {
        card = card.child(
            text(err.clone())
                .font(k.ty.small)
                .color(theme::status(k).error)
                .nowrap()
                .truncate(),
        );
    }
    card
}

fn hair_width(_k: &Kit) -> f32 {
    1.0
}

fn timing(_k: &Kit, run: &Run) -> String {
    match widgets::span(run.started_at, run.finished_at) {
        Some(ms) => duration(ms),
        None => format!("running {}", ago(now(), run.started_at)),
    }
}

fn now() -> u64 {
    crate::model::now_unix()
}

trait ProjectFacts {
    fn repo_facts(&self) -> String;
    fn checkout_facts(&self) -> String;
}

impl ProjectFacts for Project {
    fn repo_facts(&self) -> String {
        match &self.github_repo {
            Some(repo) => repo.clone(),
            None => "no GitHub remote".to_string(),
        }
    }

    fn checkout_facts(&self) -> String {
        match (&self.upstream_sha, &self.last_sha) {
            (Some(up), Some(last)) if up == last => format!("up to date at {}", short_sha(up)),
            (Some(up), _) => format!("upstream {}", short_sha(up)),
            (None, _) => "not fetched yet".to_string(),
        }
    }
}
