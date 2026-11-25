//! A project's page: what it is, its current run, and its run history.

use rune::{col, el, row, text, Cx, El, ScrollSpec, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::model::{Project, ProjectState, Run, RunStatus};
use crate::state::State;
use crate::theme;
use crate::widgets::{self, ago, duration, mono_chip, short_sha};

pub fn view(s: &State, k: &Kit, name: &str, _cx: &mut Cx<State>, hair: f32) -> El {
    let Some(project) = s.projects.iter().find(|p| p.name == name) else {
        return widgets::empty_state(
            k,
            "Project not found",
            "It is not in the daemon's config any more.",
        );
    };
    col()
        .key("project")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header(s, k, project, hair))
        .child(
            col()
                .key("project-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .px(22.0)
                .pt(16.0)
                .scroll(ScrollSpec::default())
                .child(body(s, k, project)),
        )
}

fn header(s: &State, k: &Kit, project: &Project, hair: f32) -> El {
    let state = project.state();
    let ink = theme::status(k).for_project(state);
    let name = project.name.clone();
    let busy = state == ProjectState::Running;
    let triggering = s.triggering.as_deref() == Some(project.name.as_str());
    row()
        .key("project-header")
        .w_full()
        .px(22.0)
        .py(14.0)
        .gap(12.0)
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
        .child(
            col()
                .gap(3.0)
                .grow(1.0)
                .child(
                    row()
                        .gap(9.0)
                        .center_y()
                        .child(widgets::dot(ink, 9.0))
                        .child(
                            text(project.name.clone())
                                .font(k.ty.ui_medium)
                                .color(k.c.text_base),
                        )
                        .child(widgets::project_pill(k, state)),
                )
                .child(
                    row().gap(8.0).center_y().child(
                        text(format!("{} · {}", project.branch, project.repo_facts()))
                            .font(k.ty.small)
                            .color(k.c.text_muted)
                            .nowrap()
                            .truncate(),
                    ),
                ),
        )
        .child(
            Button::new("Back")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press_tx(|s: &mut State, tx: &mut Tx| s.open_projects(tx))
                .build(k),
        )
        .child(
            Button::new(if triggering { "Starting…" } else { "Run now" })
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .disabled(triggering || busy)
                .help(if busy {
                    "Already running"
                } else {
                    "Queue a run for the current upstream commit"
                })
                .on_press_tx(move |s: &mut State, _tx: &mut Tx| s.trigger(&name))
                .build(k),
        )
}

fn body(s: &State, k: &Kit, project: &Project) -> El {
    let mut c = col().w_full().gap(18.0).pb(28.0);

    if let Some(run) = &project.current_run {
        c = c.child(current_run(k, run));
    }

    c = c.child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(text("History").font(k.ty.ui_medium).color(k.c.text_base))
            .child(el().grow(1.0))
            .children(
                s.runs_loading
                    .then(|| text("refreshing…").font(k.ty.small).color(k.c.text_faint)),
            ),
    );

    if let Some(error) = &s.runs_error {
        c = c.child(widgets::notice(k, theme::status(k).failure, error));
    }

    if s.runs.is_empty() {
        c = c.child(widgets::notice(
            k,
            k.c.text_faint,
            "No runs recorded yet for this project.",
        ));
    } else {
        let mut rows = col()
            .w_full()
            .gap(0.0)
            .radius(k.radius.lg)
            .border(1.0, k.c.border_muted);
        for (i, run) in s.runs.iter().enumerate() {
            rows = rows.child(run_row(k, run, i + 1 == s.runs.len()));
        }
        c = c.child(rows);
    }
    c
}

/// The run in progress, as a card with its steps.
fn current_run(k: &Kit, run: &Run) -> El {
    let mut steps = col().w_full().gap(0.0);
    for (i, step) in run.steps.iter().enumerate() {
        steps = steps.child(step_line(k, step, i));
    }
    col()
        .w_full()
        .padding(14.0)
        .gap(10.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_01)
        .border(1.0, theme::status(k).running.alpha(0.4))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(widgets::run_pill(k, run.status))
                .child(mono_chip(k, &short_sha(&run.sha)))
                .child(
                    text(run.subject.clone())
                        .font(k.ty.small)
                        .color(k.c.text_muted)
                        .nowrap()
                        .truncate()
                        .grow(1.0),
                )
                .child(
                    text("Running now")
                        .font(k.ty.small)
                        .color(theme::status(k).running),
                ),
        )
        .child(steps)
}

fn step_line(k: &Kit, step: &crate::model::Step, index: usize) -> El {
    let ink = theme::status(k).for_step(step.status);
    row()
        .key(format!("pstep-{index}"))
        .w_full()
        .h(26.0)
        .gap(8.0)
        .center_y()
        .child(widgets::dot(ink, 6.0))
        .child(
            text(step.name.clone())
                .font(k.ty.small)
                .color(k.c.text_base)
                .nowrap()
                .truncate()
                .grow(1.0),
        )
        .child(
            text(step.status.label())
                .font(k.ty.small)
                .color(ink)
                .nowrap(),
        )
}

fn run_row(k: &Kit, run: &Run, last: bool) -> El {
    let ink = theme::status(k).for_run(run.status);
    let id = run.id.clone();
    let timing = match widgets::span(run.started_at, run.finished_at) {
        Some(ms) => duration(ms),
        None => format!("{}…", ago(crate::model::now_unix(), run.started_at)),
    };
    let mut r = row()
        .key(format!("run-{}", run.id))
        .w_full()
        .h(44.0)
        .px(12.0)
        .gap(10.0)
        .center_y()
        .hover(k.c.hover)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_run(&id, tx))
        .cursor(rune::Cursor::Pointer)
        .child(widgets::dot(ink, 8.0))
        .child(
            text(if run.subject.is_empty() {
                "(no subject)".to_string()
            } else {
                run.subject.clone()
            })
            .font(k.ty.small)
            .color(k.c.text_base)
            .nowrap()
            .truncate()
            .grow(1.0),
        )
        .child(mono_chip(k, &short_sha(&run.sha)))
        .child(
            text(widgets::timestamp(run.started_at))
                .font(k.ty.small)
                .color(k.c.text_faint)
                .nowrap(),
        )
        .child(text(timing).font(k.ty.small).color(k.c.text_faint).nowrap())
        .child(widgets::run_pill(k, run.status));
    if let Some(failed) = &run.failed_step {
        if matches!(run.status, RunStatus::Failure | RunStatus::Error) {
            r = r.help(format!("Failed at: {failed}"));
        }
    }
    if !last {
        r = r.border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted);
    }
    r
}

trait ProjectFacts {
    fn repo_facts(&self) -> String;
}

impl ProjectFacts for Project {
    fn repo_facts(&self) -> String {
        self.github_repo
            .clone()
            .unwrap_or_else(|| "no GitHub remote".into())
    }
}
