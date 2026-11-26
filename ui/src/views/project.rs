//! A project's page: what it is, its current run, and its run history.

use rune::{col, el, row, text, Cx, El, ScrollSpec, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::model::{Project, ProjectState, Run, RunStatus, StepStatus};
use crate::state::State;
use crate::theme;
use crate::widgets::{self, ago, duration, mono_chip, short_sha};

pub fn view(s: &State, k: &Kit, name: &str, _cx: &mut Cx<State>, hair: f32, compact: bool) -> El {
    let Some(project) = s.projects.iter().find(|p| p.name == name) else {
        return widgets::empty_state(
            k,
            "Project not found",
            "It is not in the daemon's config any more.",
        )
        .key("project-missing");
    };
    col()
        .key("project")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header(s, k, project, hair, compact))
        .child(widgets::scroll_bar(
            col()
                .key("project-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .pl(if compact { 14.0 } else { 22.0 })
                .pr(if compact { 18.0 } else { 22.0 })
                .pt(16.0)
                .scroll(ScrollSpec::default())
                .child(body(s, k, project, compact)),
            k,
        ))
}

fn header(s: &State, k: &Kit, project: &Project, hair: f32, compact: bool) -> El {
    let state = project.state();
    let ink = theme::status(k).for_project(state);
    let name = project.name.clone();
    let busy = state == ProjectState::Running;
    let triggering = s.triggering.as_deref() == Some(project.name.as_str());
    let back_btn = Button::new("Back")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press_tx(|s: &mut State, tx: &mut Tx| s.open_projects(tx))
        .build(k)
        .key("project-back");
    let run_btn = Button::new(if triggering { "Starting…" } else { "Run now" })
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .disabled(triggering || busy)
        .help(if busy {
            "Already running"
        } else {
            "Queue a run for the current upstream commit"
        })
        .on_press_tx(move |s: &mut State, _tx: &mut Tx| s.trigger(&name))
        .build(k)
        .key("project-run-now");

    let subtitle = text(format!("{} · {}", project.branch, project.repo_facts()))
        .font(k.ty.small)
        .color(k.c.text_muted)
        .min_w(0.0);
    let indicator = if busy {
        rune_kit::Loader::new().size(14.0).color(ink).build(k)
    } else {
        widgets::dot(ink, 9.0)
    };

    if compact {
        return col()
            .key("project-header")
            .w_full()
            .px(14.0)
            .py(12.0)
            .gap(10.0)
            .border_sides([0.0, 0.0, hair, 0.0], k.c.border_base)
            .child(
                col()
                    .w_full()
                    .min_w(0.0)
                    .gap(4.0)
                    .child(
                        row()
                            .w_full()
                            .gap(8.0)
                            .flex_wrap()
                            .center_y()
                            .child(indicator)
                            .child(
                                text(project.name.clone())
                                    .font(k.ty.ui_medium)
                                    .color(k.c.text_base)
                                    .wrap()
                                    .min_w(0.0)
                                    .grow(1.0),
                            )
                            .child(widgets::project_pill(k, state)),
                    )
                    .child(subtitle.wrap_anywhere()),
            )
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .flex_wrap()
                    .child(back_btn.grow(1.0))
                    .child(run_btn.grow(1.0)),
            );
    }

    row()
        .key("project-header")
        .w_full()
        .px(22.0)
        .py(14.0)
        .gap(12.0)
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_base)
        .child(
            col()
                .gap(3.0)
                .min_w(0.0)
                .grow(1.0)
                .child(
                    row()
                        .w_full()
                        .gap(9.0)
                        .center_y()
                        .child(indicator)
                        .child(
                            text(project.name.clone())
                                .font(k.ty.ui_medium)
                                .color(k.c.text_base)
                                .nowrap()
                                .truncate()
                                .min_w(0.0),
                        )
                        .child(widgets::project_pill(k, state)),
                )
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .center_y()
                        .child(subtitle.nowrap().truncate()),
                ),
        )
        .child(back_btn)
        .child(run_btn)
}

fn body(s: &State, k: &Kit, project: &Project, compact: bool) -> El {
    let mut c = col().w_full().gap(18.0).pb(28.0);

    if let Some(err) = &project.last_error {
        c = c.child(widgets::notice(k, theme::status(k).error, err));
    }

    if let Some(run) = &project.current_run {
        c = c.child(current_run(k, run, compact));
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
            .border(1.0, k.c.border_base);
        for (i, run) in s.runs.iter().enumerate() {
            rows = rows.child(run_row(k, run, i + 1 == s.runs.len(), compact));
        }
        c = c.child(rows);
    }
    c
}

/// The run in progress, as a card with its steps.
fn current_run(k: &Kit, run: &Run, compact: bool) -> El {
    let mut steps = col().w_full().gap(0.0);
    for (i, step) in run.steps.iter().enumerate() {
        steps = steps.child(step_line(k, step, i, compact));
    }
    let subj = text(run.subject.clone())
        .font(k.ty.small)
        .color(k.c.text_muted);
    let head = if compact {
        col()
            .w_full()
            .gap(6.0)
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .flex_wrap()
                    .center_y()
                    .child(widgets::run_pill(k, run.status))
                    .child(mono_chip(k, &short_sha(&run.sha)))
                    .child(el().grow(1.0))
                    .child(
                        text("Running now")
                            .font(k.ty.small)
                            .color(theme::status(k).running),
                    ),
            )
            .children((!run.subject.is_empty()).then_some(subj.wrap()))
    } else {
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(widgets::run_pill(k, run.status))
            .child(mono_chip(k, &short_sha(&run.sha)))
            .child(subj.nowrap().truncate().min_w(0.0).grow(1.0))
            .child(
                text("Running now")
                    .font(k.ty.small)
                    .color(theme::status(k).running),
            )
    };
    col()
        .w_full()
        .padding(14.0)
        .gap(10.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_01)
        .border(1.0, theme::status(k).running.alpha(0.4))
        .child(head)
        .child(steps)
}

fn step_line(k: &Kit, step: &crate::model::Step, index: usize, compact: bool) -> El {
    let ink = theme::status(k).for_step(step.status);
    let indicator = if step.status == StepStatus::Running {
        rune_kit::Loader::new().size(12.0).color(ink).build(k)
    } else {
        widgets::dot(ink, 6.0)
    };
    let name_el = text(step.name.clone())
        .font(k.ty.small)
        .color(k.c.text_base)
        .min_w(0.0);
    row()
        .key(format!("pstep-{index}"))
        .w_full()
        .min_h(26.0)
        .py(if compact { 3.0 } else { 0.0 })
        .gap(8.0)
        .center_y()
        .child(indicator)
        .child(if compact {
            name_el.wrap().grow(1.0)
        } else {
            name_el.nowrap().truncate().grow(1.0)
        })
        .child(
            text(step.status.label())
                .font(k.ty.small)
                .color(ink)
                .nowrap(),
        )
}

fn run_row(k: &Kit, run: &Run, last: bool, compact: bool) -> El {
    let ink = theme::status(k).for_run(run.status);
    let id = run.id.clone();
    let timing = match widgets::span(run.started_at, run.finished_at) {
        Some(ms) => duration(ms),
        None => format!("{}…", ago(crate::model::now_unix(), run.started_at)),
    };
    let subject = if run.subject.is_empty() {
        "(no subject)".to_string()
    } else {
        run.subject.clone()
    };
    let indicator = if run.status == RunStatus::Running {
        rune_kit::Loader::new().size(13.0).color(ink).build(k)
    } else {
        widgets::dot(ink, 8.0)
    };
    let mut r = if compact {
        col()
            .key(format!("run-{}", run.id))
            .w_full()
            .min_h(56.0)
            .px(12.0)
            .py(10.0)
            .gap(6.0)
            .hover(k.c.hover)
            .press_scale(0.992)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_run(&id, tx))
            .cursor(rune::Cursor::Pointer)
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .items_start()
                    .child(if run.status == RunStatus::Running {
                        indicator.mt(3.0)
                    } else {
                        indicator.mt(5.0)
                    })
                    .child(
                        text(subject)
                            .font(k.ty.small)
                            .color(k.c.text_base)
                            .wrap()
                            .min_w(0.0)
                            .grow(1.0),
                    )
                    .child(widgets::run_pill(k, run.status)),
            )
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .flex_wrap()
                    .center_y()
                    .child(mono_chip(k, &short_sha(&run.sha)))
                    .child(
                        text(widgets::timestamp(run.started_at))
                            .font(k.ty.small)
                            .color(k.c.text_faint)
                            .nowrap(),
                    )
                    .child(text(timing).font(k.ty.small).color(k.c.text_faint).nowrap()),
            )
    } else {
        row()
            .key(format!("run-{}", run.id))
            .w_full()
            .h(44.0)
            .px(12.0)
            .gap(10.0)
            .center_y()
            .hover(k.c.hover)
            .press_scale(0.992)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_run(&id, tx))
            .cursor(rune::Cursor::Pointer)
            .child(indicator)
            .child(
                text(subject)
                    .font(k.ty.small)
                    .color(k.c.text_base)
                    .nowrap()
                    .truncate()
                    .min_w(0.0)
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
            .child(widgets::run_pill(k, run.status))
    };
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
