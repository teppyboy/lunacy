//! A run: its commits, status and duration; a step list; and the open step's log.
//!
//! An act step shows its GitHub Actions substeps as a job → steps tree, the way the Actions UI
//! does. A step skipped because of `only_on` says so in muted text, since "skipped" alone reads
//! like a problem.

use rune::{col, el, icon, row, text, Cx, El, Icon, ScrollSpec, Tx};
use rune_kit::{glyphs, Button, ButtonVariant, ControlSize, Kit};

use crate::model::{Run, Step, StepKind, StepStatus, Substep};
use crate::state::{LogMode, LogState, State};
use crate::theme;
use crate::widgets::{self, ago, duration, mono_chip, short_sha};

pub fn view(s: &State, k: &Kit, id: &str, _cx: &mut Cx<State>, hair: f32) -> El {
    let Some(run) = s.run.as_ref().filter(|r| r.id == id) else {
        if let Some(error) = &s.run_error {
            return col().w_full().h_full().child(header_shell(k, hair)).child(
                col()
                    .w_full()
                    .grow(1.0)
                    .min_h(0.0)
                    .px(22.0)
                    .pt(20.0)
                    .child(widgets::notice(k, theme::status(k).failure, error)),
            );
        }
        return col()
            .w_full()
            .h_full()
            .child(header_shell(k, hair))
            .child(widgets::empty_state(
                k,
                "Loading run…",
                "Reading the run from the daemon.",
            ));
    };

    col()
        .key("run")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header(s, k, run, hair))
        .child(
            row()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(steps_pane(s, k, run))
                .child(log_pane(s, k, run, hair)),
        )
}

fn header_shell(k: &Kit, hair: f32) -> El {
    row()
        .key("run-header")
        .w_full()
        .h(52.0)
        .px(22.0)
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
}

fn header(s: &State, k: &Kit, run: &Run, hair: f32) -> El {
    let host = s.system.as_ref().map(|x| x.os.clone());
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
    let repo = crate::views::repo_for(s, &run.project);
    if let Some(link) = github_link(run, repo.as_deref()) {
        meta = meta.child(
            Button::new("View on GitHub")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                    let _ = tx.open_url(&link);
                })
                .build(k),
        );
    }
    meta = meta.child(
        Button::new("Back")
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press_tx(|s: &mut State, tx: &mut Tx| s.back(tx))
            .build(k),
    );

    let failed = run.failed_step.clone().filter(|_| {
        matches!(
            run.status,
            crate::model::RunStatus::Failure | crate::model::RunStatus::Error
        )
    });
    let _ = &host;

    col()
        .key("run-header")
        .w_full()
        .px(22.0)
        .py(12.0)
        .gap(6.0)
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    text(run.project.clone())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base),
                )
                .child(
                    text(format!("#{}", run_id_suffix(&run.id)))
                        .font(k.ty.mono)
                        .color(k.c.text_faint),
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

/// The left pane: the step list, with act substeps under their step.
fn steps_pane(s: &State, k: &Kit, run: &Run) -> El {
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
        list = list.child(step_row(s, k, run, step, index, host.as_deref()));
    }

    col()
        .key("run-steps")
        .w(340.0)
        .shrink()
        .h_full()
        .min_h(0.0)
        .border_sides([0.0, 0.0, 0.0, 1.0], k.c.border_muted)
        .child(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec::default())
                .child(list),
        )
}

fn step_row(s: &State, k: &Kit, run: &Run, step: &Step, index: usize, host: Option<&str>) -> El {
    let selected = s.open_step == Some(index);
    let ink = theme::status(k).for_step(step.status);
    let skipped = step.status == StepStatus::Skipped;
    let reason = widgets::skipped_reason(&step.only_on, host);
    let has_substeps = !step.substeps.is_empty();
    let run_id = run.id.clone();

    let mut label_col = col().grow(1.0).gap(1.0).child(
        text(step.name.clone())
            .font(k.ty.small)
            .color(if skipped {
                k.c.text_faint
            } else {
                k.c.text_base
            })
            .nowrap()
            .truncate(),
    );
    // The whole point of the field: a host skip says why, in muted text.
    if let Some(reason) = &reason {
        label_col = label_col.child(
            text(reason.clone())
                .font(k.ty.small)
                .color(k.c.text_faint)
                .nowrap()
                .truncate(),
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
        .min_h(34.0)
        .px(14.0)
        .py(6.0)
        .gap(9.0)
        .center_y()
        .fill(if selected { k.c.layer_03 } else { k.c.layer_01 })
        .hover(if selected { k.c.layer_03 } else { k.c.hover })
        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
            if s.open_step == Some(index) {
                s.open_step = None;
            } else {
                s.open_step = Some(index);
                let mode = s
                    .logs
                    .get(&(run_id.clone(), index))
                    .and_then(|e| e.mode)
                    .unwrap_or(LogMode::Tail);
                s.request_log(&run_id, index, mode);
            }
            tx.instant();
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

/// The right pane: the open step's substeps and log.
fn log_pane(s: &State, k: &Kit, run: &Run, hair: f32) -> El {
    let Some(index) = s.open_step else {
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
    let state = s.logs.get(&(run.id.clone(), index));

    let mut head = row()
        .w_full()
        .px(16.0)
        .py(9.0)
        .gap(10.0)
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
        .child(widgets::status_glyph(k, step.status, host.as_deref()))
        .child(
            col()
                .gap(1.0)
                .grow(1.0)
                .child(
                    text(step.name.clone())
                        .font(k.ty.small_medium)
                        .color(k.c.text_base)
                        .nowrap()
                        .truncate(),
                )
                .child(
                    row().gap(6.0).children(match step.duration_ms {
                        Some(ms) => Some(text(duration(ms)).font(k.ty.small).color(k.c.text_faint)),
                        None => Some(
                            text(widgets::step_status_label(
                                step.status,
                                &step.only_on,
                                host.as_deref(),
                            ))
                            .font(k.ty.small)
                            .color(theme::status(k).for_step(step.status)),
                        ),
                    }),
                ),
        );

    // Tail vs the whole log, for a long act log.
    let mode = state.and_then(|e| e.mode).unwrap_or(LogMode::Tail);
    head = head.child(
        rune_kit::SegmentedControl::new(
            &["Tail", "Full"],
            Some(if matches!(mode, LogMode::Tail) { 0 } else { 1 }),
        )
        .fit()
        .on_pick({
            let run_id = run.id.clone();
            move |s: &mut State, picked: usize, tx: &mut Tx| {
                let mode = if picked == 0 {
                    LogMode::Tail
                } else {
                    LogMode::Full
                };
                s.request_log(&run_id, index, mode);
                tx.instant();
            }
        })
        .build(k),
    );

    let mut body = col().w_full().gap(0.0);
    if !step.substeps.is_empty() {
        body = body.child(substeps_block(k, &step.substeps, host.as_deref()));
    }
    body = body.child(log_block(k, step, state));

    col()
        .key("run-log")
        .grow(1.0)
        .min_h(0.0)
        .h_full()
        .child(head)
        .child(
            col()
                .key("run-log-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec::default())
                .child(body),
        )
}

/// The GitHub Actions tree an act step produces: one header per job, its steps indented under it.
fn substeps_block(k: &Kit, substeps: &[Substep], host: Option<&str>) -> El {
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
        .px(16.0)
        .py(10.0)
        .gap(8.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted);
    block = block.child(widgets::caption(k, "GITHUB ACTIONS"));
    for (job, steps) in jobs {
        let mut job_block = col().w_full().gap(2.0).child(
            row()
                .w_full()
                .gap(6.0)
                .center_y()
                .child(icon(&glyphs::TRIANGLE_DOWN, 12.0).color(k.c.icon_faint))
                .child(
                    text(job.to_string())
                        .font(k.ty.small_medium)
                        .color(k.c.text_base)
                        .nowrap()
                        .truncate(),
                ),
        );
        for sub in steps {
            job_block = job_block.child(substep_row(k, sub, host));
        }
        block = block.child(job_block);
    }
    block
}

fn substep_row(k: &Kit, sub: &Substep, host: Option<&str>) -> El {
    let ink = theme::status(k).for_step(sub.status);
    let mut duration = String::new();
    if let Some(ms) = sub.duration_ms {
        duration = widgets::duration(ms);
    }
    row()
        .key(format!("substep-{}", sub.name))
        .w_full()
        .min_h(22.0)
        .pl(22.0)
        .gap(7.0)
        .center_y()
        .child(widgets::status_glyph(k, sub.status, host))
        .child(
            text(sub.name.clone())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .nowrap()
                .truncate()
                .grow(1.0),
        )
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

fn log_block(k: &Kit, step: &Step, state: Option<&LogState>) -> El {
    let mut block = col()
        .key("log")
        .w_full()
        .padding(16.0)
        .pt(10.0)
        .gap(6.0)
        .child(widgets::caption(k, "OUTPUT"));

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
            block = block.child(widgets::log_body(k, &entry.text));
        }
    }
    let _ = step;
    block
}

/// Unused import guard: `Icon` names the type of `status_glyph`'s icons.
#[allow(dead_code)]
fn _icon_type(_: &'static Icon) {}
