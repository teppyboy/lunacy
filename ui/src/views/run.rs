//! A run: its commits, status and duration; a step list; and the open step's log.
//!
//! An act step shows its GitHub Actions substeps as a job → steps tree, the way the Actions UI
//! does. A step skipped because of `only_on` says so in muted text, since "skipped" alone reads
//! like a problem.

use rune::{col, el, icon, row, text, Cx, El, Icon, ScrollSpec, Spring, Transition, Tx};
use rune_kit::{glyphs, Button, ButtonVariant, ControlSize, Kit};

use crate::model::{Run, RunStatus, Step, StepKind, StepStatus, Substep};
use crate::state::{LogMode, LogState, State};
use crate::theme;
use crate::widgets::{self, ago, duration, mono_chip, short_sha};

pub fn view(s: &State, k: &Kit, id: &str, _cx: &mut Cx<State>, hair: f32, compact: bool) -> El {
    let Some(run) = s.run.as_ref().filter(|r| r.id == id) else {
        if let Some(error) = &s.run_error {
            return col()
                .key("run-error")
                .w_full()
                .h_full()
                .child(header_shell(k, hair, compact))
                .child(
                    col()
                        .w_full()
                        .grow(1.0)
                        .min_h(0.0)
                        .px(if compact { 14.0 } else { 22.0 })
                        .pt(20.0)
                        .child(widgets::notice(k, theme::status(k).failure, error)),
                );
        }
        return col()
            .key("run-loading")
            .w_full()
            .h_full()
            .child(header_shell(k, hair, compact))
            .child(widgets::empty_state(
                k,
                "Loading run…",
                "Reading the run from the daemon.",
            ));
    };

    let body = if compact {
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(steps_pane(s, k, run, hair, true))
            .child(log_pane(s, k, run, hair, true))
    } else {
        row()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(steps_pane(s, k, run, hair, false))
            .child(log_pane(s, k, run, hair, false))
    };

    col()
        .key("run")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header(s, k, run, hair, compact))
        .child(body)
}

fn header_shell(k: &Kit, hair: f32, compact: bool) -> El {
    row()
        .key("run-header")
        .w_full()
        .h(52.0)
        .px(if compact { 14.0 } else { 22.0 })
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_base)
}

fn header(s: &State, k: &Kit, run: &Run, hair: f32, compact: bool) -> El {
    let host = s.system.as_ref().map(|x| x.os.clone());
    let repo = crate::views::repo_for(s, &run.project);
    let github_btn = github_link(run, repo.as_deref()).map(|link| {
        Button::new("View on GitHub")
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                let _ = tx.open_url(&link);
            })
            .build(k)
            .key("run-github")
    });
    let back_btn = Button::new("Back")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press_tx(|s: &mut State, tx: &mut Tx| s.back(tx))
        .build(k)
        .key("run-back");

    let failed = run.failed_step.clone().filter(|_| {
        matches!(
            run.status,
            crate::model::RunStatus::Failure | crate::model::RunStatus::Error
        )
    });
    let _ = &host;

    if compact {
        let mut facts = row()
            .w_full()
            .gap(8.0)
            .flex_wrap()
            .center_y()
            .child(widgets::run_pill(k, run.status))
            .child(mono_chip(k, &short_sha(&run.sha)))
            .child(
                text(widgets::timestamp(run.started_at))
                    .font(k.ty.small)
                    .color(k.c.text_faint)
                    .nowrap(),
            )
            .children(widgets::span(run.started_at, run.finished_at).map(|ms| {
                text(duration(ms))
                    .font(k.ty.small)
                    .color(k.c.text_faint)
                    .nowrap()
            }))
            .children(run.finished_at.is_none().then(|| {
                text(format!(
                    "running {}",
                    ago(crate::model::now_unix(), run.started_at)
                ))
                .font(k.ty.small)
                .color(theme::status(k).running)
                .nowrap()
            }));
        let _ = &mut facts;

        let mut actions = row().w_full().gap(8.0).flex_wrap();
        if let Some(btn) = github_btn {
            actions = actions.child(btn.grow(1.0));
        }
        actions = actions.child(back_btn.grow(1.0));

        return col()
            .key("run-header")
            .w_full()
            .px(14.0)
            .py(12.0)
            .gap(8.0)
            .border_sides([0.0, 0.0, hair, 0.0], k.c.border_base)
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .flex_wrap()
                    .center_y()
                    .child(
                        text(run.project.clone())
                            .font(k.ty.ui_medium)
                            .color(k.c.text_base)
                            .wrap()
                            .shrink()
                            .min_w(0.0),
                    )
                    .child(
                        text(format!("#{}", run_id_suffix(&run.id)))
                            .font(k.ty.mono)
                            .color(k.c.text_faint)
                            .wrap_anywhere()
                            .shrink()
                            .min_w(0.0),
                    )
                    .child(el().grow(1.0))
                    .children(failed.map(|step| {
                        row()
                            .gap(6.0)
                            .center_y()
                            .child(text("failed at").font(k.ty.small).color(k.c.text_faint))
                            .child(widgets::pill(k, step, theme::status(k).failure))
                    })),
            )
            .child(facts)
            .children((!run.subject.is_empty()).then(|| {
                text(run.subject.clone())
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .wrap()
            }))
            .child(actions)
            .children(
                (run.status == RunStatus::Running)
                    .then(|| rune_kit::Progress::new(None).build(k).key("run-progress")),
            );
    }

    let mut meta = row().w_full().gap(8.0).center_y();
    meta = meta
        .child(widgets::run_pill(k, run.status))
        .child(mono_chip(k, &short_sha(&run.sha)))
        .children((!run.subject.is_empty()).then(|| {
            text(run.subject.clone())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .nowrap()
                .truncate()
                .min_w(0.0)
                .shrink()
        }))
        .child(
            text("·")
                .font(k.ty.small)
                .color(k.c.text_faint)
                .a11y_hidden(),
        )
        .child(
            text(widgets::timestamp(run.started_at))
                .font(k.ty.small)
                .color(k.c.text_faint)
                .nowrap(),
        )
        .children(widgets::span(run.started_at, run.finished_at).map(|ms| {
            text(duration(ms))
                .font(k.ty.small)
                .color(k.c.text_faint)
                .nowrap()
        }))
        .children(run.finished_at.is_none().then(|| {
            text(format!(
                "running {}",
                ago(crate::model::now_unix(), run.started_at)
            ))
            .font(k.ty.small)
            .color(theme::status(k).running)
            .nowrap()
        }))
        .child(el().grow(1.0));

    // "View on GitHub": the check run when the daemon made one, else the commit.
    if let Some(btn) = github_btn {
        meta = meta.child(btn);
    }
    meta = meta.child(back_btn);

    col()
        .key("run-header")
        .w_full()
        .px(22.0)
        .py(12.0)
        .gap(6.0)
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_base)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    text(run.project.clone())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base)
                        .nowrap()
                        .truncate()
                        .min_w(0.0),
                )
                .child(
                    text(format!("#{}", run_id_suffix(&run.id)))
                        .font(k.ty.mono)
                        .color(k.c.text_faint)
                        .nowrap()
                        .min_w(0.0),
                )
                .child(el().grow(1.0))
                .children(failed.map(|step| {
                    row()
                        .gap(6.0)
                        .center_y()
                        .child(text("failed at").font(k.ty.small).color(k.c.text_faint))
                        .child(widgets::pill(k, step, theme::status(k).failure))
                })),
        )
        .child(meta)
        .children(
            (run.status == RunStatus::Running)
                .then(|| rune_kit::Progress::new(None).build(k).key("run-progress")),
        )
}

/// `#4f1c2b9` reads better than the whole id.
fn run_id_suffix(id: &str) -> String {
    id.rsplit('-').next().unwrap_or(id).to_string()
}

/// `github_url` when the daemon made a check run, otherwise the commit on the project's repo.
pub fn github_link(run: &Run, repo: Option<&str>) -> Option<String> {
    if let Some(url) = run.github_url.as_ref().filter(|u| !u.is_empty()) {
        return Some(url.clone());
    }
    let repo = repo?;
    if run.sha.is_empty() {
        return None;
    }
    Some(format!("https://github.com/{repo}/commit/{}", run.sha))
}

/// The step list, on the left on desktop or stacked above the log on compact viewports.
fn steps_pane(s: &State, k: &Kit, run: &Run, hair: f32, compact: bool) -> El {
    let host = s.system.as_ref().map(|x| x.os.clone());
    let mut phase = None;
    let mut list = col().w_full().gap(0.0);
    for (index, step) in run.steps.iter().enumerate() {
        if phase != Some(step.phase) {
            phase = Some(step.phase);
            list = list.child(
                row()
                    .w_full()
                    .px(14.0)
                    .pt(12.0)
                    .pb(4.0)
                    .child(widgets::caption(k, step.phase.label())),
            );
        }
        list = list.child(step_row(s, k, run, step, index, host.as_deref(), compact));
    }

    if compact {
        let mut scroller = col()
            .key("run-steps-scroll")
            .w_full()
            .min_h(0.0)
            .scroll(ScrollSpec::default())
            .child(list);
        scroller = if s.open_step.is_some() {
            scroller.max_h(220.0)
        } else {
            scroller.grow(1.0)
        };
        let mut pane = col()
            .key("run-steps")
            .w_full()
            .border_sides([0.0, 0.0, hair, 0.0], k.c.border_base)
            .child(widgets::scroll_bar(scroller, k));
        if s.open_step.is_none() {
            pane = pane.grow(1.0).min_h(0.0);
        }
        return pane;
    }

    col()
        .key("run-steps")
        .w(340.0)
        .shrink()
        .h_full()
        .min_h(0.0)
        .border_sides([0.0, 0.0, 0.0, hair], k.c.border_base)
        .child(widgets::scroll_bar(
            col()
                .key("run-steps-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec::default())
                .child(list),
            k,
        ))
}

fn step_row(
    s: &State,
    k: &Kit,
    run: &Run,
    step: &Step,
    index: usize,
    host: Option<&str>,
    compact: bool,
) -> El {
    let selected = s.open_step == Some(index);
    let ink = theme::status(k).for_step(step.status);
    let skipped = step.status == StepStatus::Skipped;
    let only_on = effective_only_on(s, run, step);
    let reason = widgets::skipped_reason(&only_on, host);
    let has_substeps = !step.substeps.is_empty();
    let run_id = run.id.clone();

    let name_txt = text(step.name.clone()).font(k.ty.small).color(if skipped {
        k.c.text_faint
    } else {
        k.c.text_base
    });
    let mut label_col = col().grow(1.0).min_w(0.0).gap(1.0).child(if compact {
        name_txt.wrap()
    } else {
        name_txt.nowrap().truncate()
    });
    // The whole point of the field: a host skip says why, in muted text.
    if let Some(reason) = &reason {
        let r_txt = text(reason.clone()).font(k.ty.small).color(k.c.text_faint);
        label_col = label_col.child(if compact {
            r_txt.wrap()
        } else {
            r_txt.nowrap().truncate()
        });
    } else if skipped {
        label_col = label_col.child(
            text("Skipped".to_string())
                .font(k.ty.small)
                .color(k.c.text_faint),
        );
    } else if let Some(ms) = step.duration_ms {
        label_col = label_col.child(text(duration(ms)).font(k.ty.small).color(k.c.text_faint));
    } else if let Some(code) = step.exit_code.filter(|c| *c != 0) {
        label_col = label_col.child(text(format!("exit {code}")).font(k.ty.small).color(ink));
    }

    let mut right = row().gap(4.0).center_y();
    if has_substeps {
        right = right.child(
            text(format!("{}", step.substeps.len()))
                .font(k.ty.small)
                .color(k.c.text_faint)
                .a11y_hidden(),
        );
    }
    if !skipped {
        right = right.child(
            text(step.status.label())
                .font(k.ty.small)
                .color(ink)
                .nowrap(),
        );
    }

    let mut r = row()
        .key(format!("step-{index}"))
        .w_full()
        .min_h(if compact { 44.0 } else { 34.0 })
        .pl(14.0)
        .pr(18.0)
        .py(if compact { 8.0 } else { 6.0 })
        .gap(9.0)
        .center_y()
        .fill(if selected { k.c.layer_03 } else { k.c.layer_01 })
        .hover(if selected { k.c.layer_03 } else { k.c.hover })
        .press_scale(0.992)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
            if s.open_step == Some(index) {
                s.open_step = None;
                s.stick_log_pending = false;
            } else {
                s.open_step = Some(index);
                let entry = s.logs.get(&(run_id.clone(), index));
                let mode = entry.and_then(|e| e.mode).unwrap_or(LogMode::Tail);
                s.stick_log_pending = entry.is_none_or(|e| e.text.is_empty());
                s.request_log(&run_id, index, mode);
                tx.scroll_to_bottom_instant("run-log-scroll");
                s.refresh(tx);
            }
        })
        .cursor(rune::Cursor::Pointer)
        .child(widgets::status_glyph(k, step.status, host))
        .child(label_col)
        .child(right);
    if skipped {
        r = r.help(reason.unwrap_or_else(|| "Skipped".into()));
    }
    if matches!(step.kind, StepKind::Act) {
        r = r.border_sides([0.0, 0.0, 0.0, 2.0], k.c.border_muted);
    }
    r
}

/// The log pane: the open step's substeps and log, full width below steps when compact.
fn log_pane(s: &State, k: &Kit, run: &Run, hair: f32, compact: bool) -> El {
    let Some(index) = s.open_step else {
        if compact {
            return el().key("run-log-none").h(0.0);
        }
        return widgets::empty_state(
            k,
            "Pick a step",
            "Its output appears here. Act steps also show one row per GitHub Actions step.",
        );
    };
    let Some(step) = run.steps.get(index) else {
        return widgets::empty_state(k, "Pick a step", "Its output appears here.");
    };
    let host = s.system.as_ref().map(|x| x.os.clone());
    let only_on = effective_only_on(s, run, step);
    let state = s.logs.get(&(run.id.clone(), index));

    let mode = state.and_then(|e| e.mode).unwrap_or(LogMode::Tail);
    let mode_ctrl = widgets::segmented(
        k,
        compact,
        &["Tail", "Full"],
        Some(if matches!(mode, LogMode::Tail) { 0 } else { 1 }),
        {
            let run_id = run.id.clone();
            move |s: &mut State, picked: usize, tx: &mut Tx| {
                let mode = if picked == 0 {
                    LogMode::Tail
                } else {
                    LogMode::Full
                };
                s.stick_log_pending = true;
                s.request_log(&run_id, index, mode);
                tx.scroll_to_bottom_instant("run-log-scroll");
                s.refresh(tx);
            }
        },
    )
    .key("run-log-mode");

    let wrap_on = s.log_wrap;
    let wrap_btn = el()
        .key("run-log-wrap")
        .role("button")
        .label("Wrap")
        .a11y_value(if wrap_on { "on" } else { "off" })
        .help("Toggle word wrap (Alt+Z)")
        .h(if compact { 44.0 } else { 26.0 })
        .min_w(if compact { 44.0 } else { 0.0 })
        .px(if compact { 14.0 } else { 10.0 })
        .center()
        .radius(k.radius.md)
        .fill(if wrap_on {
            k.c.text_accent.alpha(0.14)
        } else {
            k.c.layer_01
        })
        .border(
            1.0,
            if wrap_on {
                k.c.text_accent.alpha(0.55)
            } else {
                k.c.border_muted
            },
        )
        .hover(k.c.hover)
        .press_scale(0.98)
        .cursor(rune::Cursor::Pointer)
        .on_press_tx(|s: &mut State, tx: &mut Tx| s.toggle_log_wrap(tx))
        .child(
            text("Wrap")
                .font(k.ty.small_medium)
                .color(if wrap_on {
                    k.c.text_accent
                } else {
                    k.c.text_muted
                })
                .nowrap(),
        );

    let step_title = text(step.name.clone())
        .font(k.ty.small_medium)
        .color(k.c.text_base);
    let title_block = col()
        .gap(1.0)
        .grow(1.0)
        .min_w(0.0)
        .child(if compact {
            step_title.wrap()
        } else {
            step_title.nowrap().truncate()
        })
        .child(
            row().gap(6.0).children(match step.duration_ms {
                Some(ms) => Some(text(duration(ms)).font(k.ty.small).color(k.c.text_faint)),
                None => Some(
                    text(widgets::step_status_label(
                        step.status,
                        &only_on,
                        host.as_deref(),
                    ))
                    .font(k.ty.small)
                    .color(theme::status(k).for_step(step.status))
                    .wrap()
                    .shrink(),
                ),
            }),
        );

    let mut head = row()
        .w_full()
        .px(if compact { 14.0 } else { 16.0 })
        .py(9.0)
        .gap(10.0)
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_base);
    if compact {
        head = head.flex_wrap();
    }
    head = head
        .child(widgets::status_glyph(k, step.status, host.as_deref()))
        .child(title_block)
        .child(row().gap(6.0).center_y().child(wrap_btn).child(mode_ctrl));

    let mut body = col()
        .key(format!("log-step-{}-{index}", run.id))
        .w_full()
        .gap(0.0)
        .transition(Transition::asymmetric(
            Transition::opacity(),
            Transition::none(),
        ))
        .transition_motion(rune::tok::motion::SNAP, Spring::NONE);
    if s.log_wrap {
        body = body.min_w(0.0);
    }
    if !step.substeps.is_empty() {
        body = body.child(substeps_block(k, &step.substeps, host.as_deref(), compact));
    }
    body = body.child(log_block(k, step, state, s.log_wrap));

    let log_viewport = col()
        .key("run-log-scroll")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec::stick_bottom());
    let log_scroll = if s.log_wrap {
        widgets::scroll_bar(log_viewport.child(body), k)
    } else {
        widgets::scroll_bars_xy(log_viewport.scroll_xy().child(body), k)
    };

    col()
        .key("run-log")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .min_w(0.0)
        .h_full()
        .child(head)
        .child(log_scroll)
}

/// The GitHub Actions tree an act step produces: one header per job, its steps indented under it.
fn substeps_block(k: &Kit, substeps: &[Substep], host: Option<&str>, compact: bool) -> El {
    let mut jobs: Vec<(&str, Vec<&Substep>)> = Vec::new();
    for sub in substeps {
        match jobs.iter_mut().find(|(job, _)| *job == sub.job.as_str()) {
            Some((_, steps)) => steps.push(sub),
            None => jobs.push((sub.job.as_str(), vec![sub])),
        }
    }
    let mut block = col()
        .key("substeps")
        .w_full()
        .pl(if compact { 14.0 } else { 16.0 })
        .pr(18.0)
        .py(10.0)
        .gap(8.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted);
    block = block.child(widgets::caption(k, "GITHUB ACTIONS"));
    for (job_idx, (job, steps)) in jobs.into_iter().enumerate() {
        let job_txt = text(job.to_string())
            .font(k.ty.small_medium)
            .color(k.c.text_base)
            .min_w(0.0);
        let mut job_block = col()
            .key(format!("substeps-job-{job_idx}"))
            .w_full()
            .gap(2.0)
            .child(
                row()
                    .w_full()
                    .gap(6.0)
                    .center_y()
                    .child(icon(&glyphs::TRIANGLE_DOWN, 12.0).color(k.c.icon_faint))
                    .child(if compact {
                        job_txt.wrap_anywhere().shrink()
                    } else {
                        job_txt.wrap().shrink()
                    }),
            );
        for (sub_idx, sub) in steps.into_iter().enumerate() {
            job_block = job_block.child(substep_row(k, sub, sub_idx, host, compact));
        }
        block = block.child(job_block);
    }
    block
}

fn substep_row(k: &Kit, sub: &Substep, index: usize, host: Option<&str>, compact: bool) -> El {
    let ink = theme::status(k).for_step(sub.status);
    let mut duration = String::new();
    if let Some(ms) = sub.duration_ms {
        duration = widgets::duration(ms);
    }
    let name_el = text(sub.name.clone())
        .font(k.ty.small)
        .color(k.c.text_muted)
        .min_w(0.0);
    row()
        .key(format!("substep-{index}-{}", sub.name))
        .w_full()
        .min_h(22.0)
        .pl(if compact { 16.0 } else { 22.0 })
        .gap(7.0)
        .center_y()
        .child(widgets::status_glyph(k, sub.status, host))
        .child(if compact {
            name_el.wrap_anywhere().grow(1.0)
        } else {
            name_el.nowrap().truncate().grow(1.0)
        })
        .children((!duration.is_empty()).then(|| {
            text(duration)
                .font(k.ty.small)
                .color(k.c.text_faint)
                .nowrap()
        }))
        .children(
            (sub.status == StepStatus::Running)
                .then(|| text("running").font(k.ty.small).color(ink)),
        )
}

fn log_block(k: &Kit, step: &Step, state: Option<&LogState>, wrap: bool) -> El {
    let mut block = col()
        .key("log")
        .w_full()
        .pl(16.0)
        .pr(18.0)
        .pt(10.0)
        .pb(if wrap { 16.0 } else { 18.0 })
        .gap(6.0)
        .child(widgets::caption(k, "OUTPUT"));
    if wrap {
        block = block.min_w(0.0);
    }

    match state {
        None => {
            block = block.child(text("Reading…").font(k.ty.small).color(k.c.text_faint));
        }
        Some(entry) if entry.error.as_ref().is_some_and(|e| !e.is_empty()) => {
            block = block.child(widgets::notice(
                k,
                theme::status(k).failure,
                entry.error.as_deref().unwrap_or(""),
            ));
        }
        Some(entry) if entry.text.is_empty() => {
            block = block.child(
                text(if entry.loading {
                    "Reading…"
                } else {
                    "No output."
                })
                .font(k.ty.small)
                .color(k.c.text_faint),
            );
        }
        Some(entry) => {
            block = block.child(widgets::log_body(k, &entry.text, wrap));
        }
    }
    let _ = step;
    block
}

fn effective_only_on(s: &State, run: &Run, step: &Step) -> Vec<String> {
    if !step.only_on.is_empty() {
        return step.only_on.clone();
    }
    if step.status != StepStatus::Skipped || !s.editor.loaded {
        return Vec::new();
    }
    let phase_key = match step.phase {
        crate::model::Phase::Ci => "ci",
        crate::model::Phase::Deploy => "deploy",
    };
    let projects = crate::paths::array(&s.editor.base, "project");
    let Some(project) = projects
        .iter()
        .find(|p| p.get("name").and_then(|v| v.as_str()) == Some(run.project.as_str()))
    else {
        return Vec::new();
    };
    let steps = project
        .get(phase_key)
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    steps
        .iter()
        .find(|cfg| cfg.get("name").and_then(|v| v.as_str()) == Some(step.name.as_str()))
        .and_then(|cfg| cfg.get("only_on"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Unused import guard: `Icon` names the type of `status_glyph`'s icons.
#[allow(dead_code)]
fn _icon_type(_: &'static Icon) {}
