//! The projects list: one row per watched repository, newest state at a glance.

use rune::{col, el, row, text, Cx, El, ScrollSpec, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::model::{Project, ProjectState, Run};
use crate::state::State;
use crate::theme;
use crate::widgets::{self, ago, duration, mono_chip, short_sha};

pub fn view(s: &State, k: &Kit, _cx: &mut Cx<State>, hair: f32, compact: bool) -> El {
    let mut list = col().w_full().gap(8.0).pb(24.0);
    for project in &s.projects {
        list = list.child(card_row(s, k, project, compact));
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
        .child(header(s, k, hair, compact))
        .child(widgets::scroll_bar(
            col()
                .key("projects-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .pl(if compact { 14.0 } else { 22.0 })
                .pr(if compact { 18.0 } else { 22.0 })
                .pt(16.0)
                .scroll(ScrollSpec::default())
                .child(body),
            k,
        ))
}

fn header(s: &State, k: &Kit, hair: f32, compact: bool) -> El {
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
    let force_switch = rune_kit::Switch::new(s.force)
        .label("Force")
        .hide_label()
        .on_toggle_tx(|s: &mut State, _tx: &mut Tx| {
            s.force = !s.force;
        })
        .build(k)
        .min_h(if compact { 44.0 } else { 0.0 })
        .key("projects-force");
    let settings_btn = Button::new("Settings")
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .on_press_tx(|s: &mut State, tx: &mut Tx| {
            s.open_settings(crate::state::SettingsPage::Projects, tx)
        })
        .build(k)
        .key("projects-settings");

    if compact {
        return col()
            .key("projects-header")
            .w_full()
            .px(14.0)
            .py(12.0)
            .gap(10.0)
            .fill(k.c.layer_01)
            .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
            .child(
                row()
                    .w_full()
                    .gap(10.0)
                    .center_y()
                    .child(
                        col()
                            .gap(1.0)
                            .grow(1.0)
                            .child(text("Projects").font(k.ty.ui_medium).color(k.c.text_base))
                            .child(text(counts).font(k.ty.small).color(k.c.text_muted)),
                    )
                    .child(settings_btn),
            )
            .child(
                row()
                    .w_full()
                    .min_h(44.0)
                    .gap(10.0)
                    .center_y()
                    .child(force_switch)
                    .child(
                        text("Force triggers")
                            .font(k.ty.small)
                            .color(k.c.text_muted),
                    ),
            );
    }

    let _ = hair;
    row()
        .key("projects-header")
        .w_full()
        .px(22.0)
        .py(14.0)
        .gap(12.0)
        .center_y()
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
        .child(
            col()
                .gap(1.0)
                .child(text("Projects").font(k.ty.ui_medium).color(k.c.text_base))
                .child(text(counts).font(k.ty.small).color(k.c.text_muted)),
        )
        .child(el().grow(1.0))
        .child(force_switch)
        .child(
            text("Force triggers")
                .font(k.ty.small)
                .color(k.c.text_muted),
        )
        .child(settings_btn)
}

/// One project: name, remote, its latest run and what it is doing now.
fn card_row(s: &State, k: &Kit, project: &Project, compact: bool) -> El {
    let state = project.state();
    let ink = theme::status(k).for_project(state);
    let run = project.current_run.as_ref().or(project.last_run.as_ref());
    let name = project.name.clone();
    let triggering = s.triggering.as_deref() == Some(project.name.as_str());
    let directly = project.github_repo.clone();

    let mut facts = if compact {
        row().w_full().gap(8.0).flex_wrap().center_y()
    } else {
        row().w_full().gap(14.0).center_y()
    };
    if let Some(run) = run {
        let subj = text(if run.subject.is_empty() {
            "no commit subject".to_string()
        } else {
            run.subject.clone()
        })
        .font(k.ty.small)
        .color(k.c.text_muted);
        let subj = if compact {
            subj.wrap().min_w(0.0).grow(1.0)
        } else {
            subj.nowrap().truncate().min_w(0.0).grow(1.0)
        };
        let ref_badge = widgets::run_ref_badge(k, run, Some(&project.branch));
        let deploy_only_pill = run
            .deploy_only
            .then(|| widgets::pill(k, "Deploy only", theme::status(k).running));
        facts = facts
            .child(widgets::run_pill(k, run.status))
            .children(ref_badge)
            .children(deploy_only_pill)
            .child(
                row()
                    .gap(6.0)
                    .center_y()
                    .grow(1.0)
                    .min_w(0.0)
                    .child(mono_chip(k, &short_sha(&run.sha)))
                    .child(subj),
            );
    } else {
        facts = facts.child(widgets::pill(k, "Never run", k.c.text_faint));
    }

    let cancel_btn = (state == ProjectState::Running).then(|| {
        let current_run_id = project.current_run.as_ref().map(|cur| cur.id.clone());
        let cancelling = match current_run_id.as_deref() {
            Some(id) => {
                s.cancelling.as_deref() == Some(id)
                    || s.cancelling.as_deref() == Some(project.name.as_str())
            }
            None => s.cancelling.as_deref() == Some(project.name.as_str()),
        };
        let project_name = project.name.clone();
        Button::new(if cancelling {
            "Cancelling…"
        } else {
            "Cancel"
        })
        .variant(ButtonVariant::Danger)
        .control_size(ControlSize::Small)
        .disabled(cancelling)
        .help("Stop this running pipeline")
        .on_press_tx(move |s: &mut State, tx: &mut Tx| match &current_run_id {
            Some(run_id) => s.cancel_run(run_id, tx),
            None => s.cancel_project(&project_name, tx),
        })
        .build(k)
        .key(format!("cancel-{}", project.name))
    });

    let run_btn = Button::new(if triggering { "Starting…" } else { "Run now" })
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
        .key(format!("run-now-{}", project.name));

    let mut meta_left = row().gap(8.0).flex_wrap().center_y();
    if project.pending_update() {
        meta_left = meta_left.child(widgets::pill(k, "Update waiting", theme::status(k).running));
    }
    if !project.open_prs.is_empty() {
        let count = project.open_prs.len();
        meta_left = meta_left.child(widgets::pill(
            k,
            format!("{count} PR{}", if count == 1 { "" } else { "s" }),
            k.c.text_accent,
        ));
    }
    if let Some(run) = run {
        meta_left = meta_left.child(
            text(timing(k, run))
                .font(k.ty.small)
                .color(k.c.text_faint)
                .nowrap(),
        );
    }

    let indicator = if state == ProjectState::Running {
        rune_kit::Loader::new().size(13.0).color(ink).build(k)
    } else {
        widgets::dot(ink, 8.0)
    };
    let name_el = text(project.name.clone())
        .font(k.ty.ui_medium)
        .color(k.c.text_base)
        .min_w(0.0);
    let branch_el = text(project.branch.clone())
        .font(k.ty.small)
        .color(k.c.text_faint)
        .min_w(0.0);
    let mut title_row = row().w_full().gap(10.0).center_y();
    if compact {
        title_row = title_row.flex_wrap();
    }
    title_row = title_row
        .child(indicator)
        .child(if compact {
            name_el.wrap().shrink()
        } else {
            name_el.nowrap().truncate()
        })
        .child(if compact {
            branch_el.wrap().shrink()
        } else {
            branch_el.nowrap().truncate()
        })
        .child(el().grow(1.0));
    if !compact {
        title_row = title_row.children(directly.map(|repo| {
            text(repo)
                .font(k.ty.small)
                .color(k.c.text_faint)
                .nowrap()
                .truncate()
        }));
    }
    title_row = title_row.child(widgets::project_pill(k, state));

    let mut card = col()
        .key(format!("project-{}", project.name))
        .w_full()
        .padding(14.0)
        .gap(if compact { 10.0 } else { 8.0 })
        .radius(k.radius.lg)
        .fill(k.c.layer_02)
        .border(hair_width(k), k.c.border_base)
        .hover(k.c.layer_03)
        .press_scale(0.992)
        .on_press_tx({
            let name = project.name.clone();
            move |s: &mut State, tx: &mut Tx| s.open_project(&name, tx)
        })
        .cursor(rune::Cursor::Pointer)
        .child(title_row);

    if compact {
        let mut actions = row().gap(8.0).flex_wrap().center_y();
        if let Some(btn) = cancel_btn {
            actions = actions.child(btn);
        }
        actions = actions.child(run_btn);
        card = card.child(facts).child(
            row()
                .w_full()
                .gap(8.0)
                .flex_wrap()
                .center_y()
                .child(meta_left.grow(1.0))
                .child(actions),
        );
    } else {
        let mut right = meta_left;
        if let Some(btn) = cancel_btn {
            right = right.child(btn);
        }
        right = right.child(run_btn);
        card = card.child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(facts.grow(1.0).min_w(0.0))
                .child(right),
        );
    }

    let repo_line = text(format!(
        "{} · {}",
        project.repo_facts(),
        project.checkout_facts()
    ))
    .font(k.ty.small)
    .color(k.c.text_faint);
    card = card.child(row().w_full().gap(8.0).center_y().child(if compact {
        repo_line.wrap_anywhere().shrink()
    } else {
        repo_line.nowrap().truncate()
    }));
    if let Some(err) = &project.last_error {
        let err_el = text(err.clone())
            .font(k.ty.small)
            .color(theme::status(k).error);
        card = card.child(if compact {
            err_el.wrap()
        } else {
            err_el.nowrap().truncate()
        });
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
