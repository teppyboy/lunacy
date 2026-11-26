//! A project's page: what it is, its current run, and its run history.

use rune::{col, el, icon, row, text, Cx, El, ScrollSpec, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::icons;
use crate::model::{Phase, PrSummary, Project, ProjectState, Run, RunStatus, StepStatus};
use crate::state::State;
use crate::theme;
use crate::widgets::{self, ago, duration};

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

fn has_deploy_steps(s: &State, project: &Project) -> bool {
    if let Some(report) = &s.deploy_checks {
        if let Some(p) = report.projects.iter().find(|p| p.name == project.name) {
            return p.deploy_total_steps > 0;
        }
    }
    if s.editor.loaded {
        let projects = crate::paths::array(&s.editor.base, "project");
        if let Some(cfg) = projects
            .iter()
            .find(|p| p.get("name").and_then(|v| v.as_str()) == Some(project.name.as_str()))
        {
            return cfg
                .get("deploy")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| !arr.is_empty());
        }
    }
    if let Some(run) = project.current_run.as_ref().or(project.last_run.as_ref()) {
        if !run.steps.is_empty() {
            return run.steps.iter().any(|st| st.phase == Phase::Deploy);
        }
    }
    // Config not loaded yet: allow clicking so the daemon handles it cleanly.
    true
}

fn header(s: &State, k: &Kit, project: &Project, hair: f32, compact: bool) -> El {
    let _ = hair;
    let state = project.state();
    let ink = theme::status(k).for_project(state);
    let name = project.name.clone();
    let busy = state == ProjectState::Running;
    let active_runs = project.active_run_list();
    let active_count = project.active_run_count();
    let at_limit = project.at_concurrency_limit();
    let limit_help = if project.max_concurrent_runs > 1 {
        "Concurrency limit reached"
    } else {
        "Already running"
    };
    let triggering = s.triggering.as_deref() == Some(project.name.as_str());
    let deploying = s.force_deploying.as_deref() == Some(project.name.as_str());
    let cancel_btn = busy.then(|| {
        let single_run_id = (active_count <= 1)
            .then(|| active_runs.first().map(|cur| cur.id.clone()))
            .flatten();
        let cancelling = match single_run_id.as_deref() {
            Some(id) => {
                s.cancelling.as_deref() == Some(id)
                    || s.cancelling.as_deref() == Some(project.name.as_str())
            }
            None => s.cancelling.as_deref() == Some(project.name.as_str()),
        };
        let project_name = project.name.clone();
        Button::new(if cancelling {
            "Cancelling…"
        } else if active_count > 1 {
            "Cancel all"
        } else {
            "Cancel"
        })
        .icon(&icons::SQUARE)
        .variant(ButtonVariant::Danger)
        .control_size(ControlSize::Small)
        .disabled(cancelling)
        .help(if active_count > 1 {
            "Stop all active runs for this project"
        } else {
            "Stop the currently running pipeline"
        })
        .on_press_tx(move |s: &mut State, tx: &mut Tx| match &single_run_id {
            Some(run_id) => s.cancel_run(run_id, tx),
            None => s.cancel_project(&project_name, tx),
        })
        .build(k)
        .key("project-cancel")
    });
    let force_deploy_btn = has_deploy_steps(s, project).then(|| {
        let deploy_name = project.name.clone();
        Button::new(if deploying {
            "Deploying…"
        } else {
            "Force deploy"
        })
        .icon(&icons::ROCKET)
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .disabled(deploying || at_limit)
        .help(if at_limit {
            limit_help
        } else {
            "Queue a deploy-only run for the current commit"
        })
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.force_deploy_project(&deploy_name, tx))
        .build(k)
        .key("project-force-deploy")
    });
    let back_btn = Button::new("Back")
        .icon(&icons::ARROW_LEFT)
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press_tx(|s: &mut State, tx: &mut Tx| s.open_projects(tx))
        .build(k)
        .key("project-back");
    let run_btn = Button::new(if triggering { "Starting…" } else { "Run now" })
        .icon(&icons::PLAY)
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .disabled(triggering || at_limit)
        .help(if at_limit {
            limit_help
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
    let status_pill = if active_count > 1 {
        widgets::pill_with_icon(
            k,
            &icons::ACTIVITY,
            format!("{active_count} running"),
            theme::status(k).running,
        )
        .key("project-active-count")
    } else {
        widgets::project_pill(k, state)
    };

    if compact {
        return col()
            .key("project-header")
            .w_full()
            .px(14.0)
            .py(12.0)
            .gap(10.0)
            .fill(k.c.layer_01)
            .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
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
                            .children((!project.enabled).then(|| widgets::paused_pill(k)))
                            .child(status_pill),
                    )
                    .child(subtitle.wrap_anywhere()),
            )
            .child({
                let mut actions = row()
                    .w_full()
                    .gap(8.0)
                    .flex_wrap()
                    .child(back_btn.grow(1.0));
                if let Some(btn) = cancel_btn {
                    actions = actions.child(btn.grow(1.0));
                }
                if let Some(btn) = force_deploy_btn {
                    actions = actions.child(btn.grow(1.0));
                }
                actions.child(run_btn.grow(1.0))
            });
    }

    let mut hdr = row()
        .key("project-header")
        .w_full()
        .px(22.0)
        .py(14.0)
        .gap(12.0)
        .center_y()
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
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
                        .children((!project.enabled).then(|| widgets::paused_pill(k)))
                        .child(status_pill),
                )
                .child(
                    row()
                        .w_full()
                        .gap(6.0)
                        .center_y()
                        .child(
                            icon(&icons::GIT_BRANCH, 12.0)
                                .color(k.c.icon_faint)
                                .a11y_hidden(),
                        )
                        .child(subtitle.nowrap().truncate()),
                ),
        )
        .child(back_btn);
    if let Some(btn) = cancel_btn {
        hdr = hdr.child(btn);
    }
    if let Some(btn) = force_deploy_btn {
        hdr = hdr.child(btn);
    }
    hdr.child(run_btn)
}

fn body(s: &State, k: &Kit, project: &Project, compact: bool) -> El {
    let mut c = col().w_full().gap(18.0).pb(28.0);

    if let Some(err) = &project.last_error {
        c = c.child(widgets::notice(k, theme::status(k).error, err));
    }

    let active_runs = project.active_run_list();
    let multi_active = active_runs.len() > 1;
    for (idx, run) in active_runs.iter().enumerate() {
        c = c.child(current_run(s, k, project, run, idx, multi_active, compact));
    }

    if !project.open_prs.is_empty() {
        c = c.child(open_prs_section(k, &project.open_prs, compact));
    }

    c = c.child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(
                icon(&icons::HISTORY, 15.0)
                    .color(k.c.icon_muted)
                    .a11y_hidden(),
            )
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
            .fill(k.c.layer_02)
            .border(1.0, k.c.border_base)
            .border_layout()
            .clip_rounded(k.radius.lg);
        let project_at_limit = project.at_concurrency_limit();
        for (i, run) in s.runs.iter().enumerate() {
            rows = rows.child(run_row(
                s,
                k,
                &project.branch,
                run,
                project_at_limit,
                i + 1 == s.runs.len(),
                compact,
            ));
        }
        c = c.child(rows);
    }
    c
}

fn open_prs_section(k: &Kit, prs: &[PrSummary], compact: bool) -> El {
    let mut list = col()
        .w_full()
        .gap(0.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base)
        .border_layout()
        .clip_rounded(k.radius.lg);
    for (i, pr) in prs.iter().enumerate() {
        let last = i + 1 == prs.len();
        let status_pill = match pr.status {
            Some(st) => widgets::run_pill(k, st),
            None => widgets::pill(k, "Pending", k.c.text_faint),
        };
        let branch_flow = if pr.base_ref.is_empty() {
            pr.head_ref.clone()
        } else {
            format!("{} → {}", pr.head_ref, pr.base_ref)
        };
        let mut r = if compact {
            col()
                .key(format!("project-pr-{}", pr.number))
                .w_full()
                .px(12.0)
                .py(10.0)
                .gap(6.0)
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .flex_wrap()
                        .center_y()
                        .child(widgets::pr_pill(k, pr.number))
                        .child(
                            text(pr.title.clone())
                                .font(k.ty.small_medium)
                                .color(k.c.text_base)
                                .wrap()
                                .min_w(0.0)
                                .grow(1.0),
                        )
                        .child(status_pill),
                )
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .flex_wrap()
                        .center_y()
                        .child(
                            text(branch_flow)
                                .font(k.ty.mono)
                                .color(k.c.text_muted)
                                .wrap_anywhere()
                                .min_w(0.0),
                        )
                        .child(widgets::sha_chip(k, &pr.head_sha)),
                )
        } else {
            row()
                .key(format!("project-pr-{}", pr.number))
                .w_full()
                .min_h(40.0)
                .px(12.0)
                .py(8.0)
                .gap(10.0)
                .center_y()
                .child(widgets::pr_pill(k, pr.number))
                .child(
                    text(pr.title.clone())
                        .font(k.ty.small_medium)
                        .color(k.c.text_base)
                        .nowrap()
                        .truncate()
                        .min_w(0.0)
                        .grow(1.0),
                )
                .child(
                    text(branch_flow)
                        .font(k.ty.mono)
                        .color(k.c.text_muted)
                        .nowrap()
                        .truncate()
                        .min_w(0.0),
                )
                .child(widgets::sha_chip(k, &pr.head_sha))
                .child(status_pill)
        };
        if !last {
            r = r.border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted);
        }
        list = list.child(r);
    }

    col()
        .key("project-open-prs")
        .w_full()
        .gap(8.0)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    icon(&icons::GIT_PULL_REQUEST, 15.0)
                        .color(k.c.icon_muted)
                        .a11y_hidden(),
                )
                .child(
                    text("Open pull requests")
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base),
                )
                .child(widgets::pill(k, format!("{}", prs.len()), k.c.text_accent)),
        )
        .child(list)
}

/// One run in progress, as a card with clickable header, optional per-run Cancel, and its steps.
fn current_run(
    s: &State,
    k: &Kit,
    project: &Project,
    run: &Run,
    active_idx: usize,
    multi_active: bool,
    compact: bool,
) -> El {
    let mut steps = col().w_full().gap(0.0);
    for (i, step) in run.steps.iter().enumerate() {
        steps = steps.child(step_line(k, step, active_idx, i, compact));
    }
    let subj = text(run.subject.clone())
        .font(k.ty.small)
        .color(k.c.text_muted);
    let ref_badge = widgets::run_ref_badge(k, run, Some(&project.branch));
    let deploy_only_pill = run.deploy_only.then(|| widgets::deploy_only_pill(k));
    let trigger_key = if active_idx == 0 {
        "current-run-trigger".to_string()
    } else {
        format!("current-run-trigger-{active_idx}")
    };
    let reason_key = if active_idx == 0 {
        "current-run-trigger-reason".to_string()
    } else {
        format!("current-run-trigger-reason-{active_idx}")
    };
    let trigger_pill = widgets::trigger_pill(k, run, trigger_key);
    let trigger_reason = run.trigger_display().map(|reason| {
        let t = text(reason)
            .key(reason_key)
            .font(k.ty.small)
            .color(k.c.text_faint)
            .min_w(0.0);
        if compact {
            t.wrap()
        } else {
            t.nowrap().truncate()
        }
    });
    let per_run_cancel = multi_active.then(|| {
        let run_id = run.id.clone();
        let cancelling = s.cancelling.as_deref() == Some(run.id.as_str())
            || s.cancelling.as_deref() == Some(project.name.as_str());
        Button::new(if cancelling {
            "Cancelling…"
        } else {
            "Cancel"
        })
        .icon(&icons::SQUARE)
        .variant(ButtonVariant::Danger)
        .control_size(ControlSize::Small)
        .disabled(cancelling)
        .help("Stop this active run")
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.cancel_run(&run_id, tx))
        .build(k)
        .key(format!("current-run-cancel-{}", run.id))
    });
    let open_id = run.id.clone();
    let open_info = if compact {
        col()
            .key(format!("current-run-open-{}", run.id))
            .role("button")
            .w_full()
            .min_h(44.0)
            .gap(6.0)
            .radius(k.radius.sm)
            .hover(k.c.layer_03)
            .cursor(rune::Cursor::Pointer)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_run(&open_id, tx))
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .flex_wrap()
                    .center_y()
                    .child(widgets::run_pill(k, run.status))
                    .children(ref_badge)
                    .children(deploy_only_pill)
                    .children(trigger_pill)
                    .child(widgets::sha_chip(k, &run.sha))
                    .child(el().grow(1.0))
                    .child(
                        text("Running now")
                            .font(k.ty.small)
                            .color(theme::status(k).running),
                    ),
            )
            .children((!run.subject.is_empty()).then_some(subj.wrap()))
            .children(trigger_reason)
    } else {
        col()
            .key(format!("current-run-open-{}", run.id))
            .grow(1.0)
            .min_w(0.0)
            .gap(4.0)
            .radius(k.radius.sm)
            .hover(k.c.layer_03)
            .cursor(rune::Cursor::Pointer)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_run(&open_id, tx))
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(widgets::run_pill(k, run.status))
                    .children(ref_badge)
                    .children(deploy_only_pill)
                    .children(trigger_pill)
                    .child(widgets::sha_chip(k, &run.sha))
                    .child(subj.nowrap().truncate().min_w(0.0).grow(1.0))
                    .child(
                        text("Running now")
                            .font(k.ty.small)
                            .color(theme::status(k).running)
                            .nowrap(),
                    ),
            )
            .children(trigger_reason)
    };
    let head = if compact {
        col()
            .w_full()
            .gap(8.0)
            .child(open_info)
            .children(per_run_cancel)
    } else {
        row()
            .w_full()
            .gap(10.0)
            .center_y()
            .child(open_info)
            .children(per_run_cancel)
    };
    col()
        .key(format!("current-run-{}", run.id))
        .w_full()
        .padding(14.0)
        .gap(10.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_02)
        .border(1.0, theme::status(k).running.alpha(0.45))
        .child(head)
        .child(steps)
}

fn step_line(
    k: &Kit,
    step: &crate::model::Step,
    active_idx: usize,
    index: usize,
    compact: bool,
) -> El {
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
    let key = if active_idx == 0 {
        format!("pstep-{index}")
    } else {
        format!("pstep-{active_idx}-{index}")
    };
    row()
        .key(key)
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

fn run_row(
    s: &State,
    k: &Kit,
    default_branch: &str,
    run: &Run,
    project_running: bool,
    last: bool,
    compact: bool,
) -> El {
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
    let ref_badge = widgets::run_ref_badge(k, run, Some(default_branch));
    let deploy_only_pill = run.deploy_only.then(|| widgets::deploy_only_pill(k));
    let trigger_pill = widgets::trigger_pill(k, run, format!("run-trigger-{}", run.id));
    let trigger_reason = run
        .trigger_reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let skipped_note = run
        .deploy_skipped_reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let subtitle_note = match (trigger_reason, skipped_note) {
        (Some(tr), Some(sk)) => Some(format!("{tr} · {sk}")),
        (Some(tr), None) => Some(tr.to_string()),
        (None, Some(sk)) => Some(sk.to_string()),
        (None, None) => None,
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
            .children(subtitle_note.map(|note| {
                text(note)
                    .key(format!("run-subtitle-{}", run.id))
                    .font(k.ty.small)
                    .color(k.c.text_faint)
                    .wrap()
                    .min_w(0.0)
            }))
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .flex_wrap()
                    .center_y()
                    .children(ref_badge)
                    .children(deploy_only_pill)
                    .children(trigger_pill)
                    .child(widgets::sha_chip(k, &run.sha))
                    .child(
                        text(widgets::timestamp(run.started_at))
                            .font(k.ty.small)
                            .color(k.c.text_faint)
                            .nowrap(),
                    )
                    .child(text(timing).font(k.ty.small).color(k.c.text_faint).nowrap()),
            )
    } else {
        let retry_btn = (!project_running
            && matches!(
                run.status,
                RunStatus::Failure | RunStatus::Error | RunStatus::Cancelled
            ))
        .then(|| {
            let retrying_this = s.retrying.as_deref() == Some(run.id.as_str());
            let retry_id = run.id.clone();
            Button::new(if retrying_this {
                "Retrying…"
            } else {
                "Retry"
            })
            .icon(&icons::ROTATE_CW)
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Mini)
            .disabled(s.retrying.is_some())
            .help("Re-run CI for this commit")
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.retry_run(&retry_id, tx))
            .build(k)
            .key(format!("run-retry-{}", run.id))
        });
        row()
            .key(format!("run-{}", run.id))
            .w_full()
            .min_h(44.0)
            .px(12.0)
            .py(6.0)
            .gap(10.0)
            .center_y()
            .hover(k.c.hover)
            .press_scale(0.992)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_run(&id, tx))
            .cursor(rune::Cursor::Pointer)
            .child(indicator)
            .children(ref_badge)
            .children(deploy_only_pill)
            .children(trigger_pill)
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(1.0)
                    .child(
                        text(subject)
                            .font(k.ty.small)
                            .color(k.c.text_base)
                            .nowrap()
                            .truncate()
                            .min_w(0.0),
                    )
                    .children(subtitle_note.map(|note| {
                        text(note)
                            .key(format!("run-subtitle-{}", run.id))
                            .font(k.ty.small)
                            .color(k.c.text_faint)
                            .nowrap()
                            .truncate()
                            .min_w(0.0)
                    })),
            )
            .child(widgets::sha_chip(k, &run.sha))
            .child(
                text(widgets::timestamp(run.started_at))
                    .font(k.ty.small)
                    .color(k.c.text_faint)
                    .nowrap(),
            )
            .child(text(timing).font(k.ty.small).color(k.c.text_faint).nowrap())
            .child(widgets::run_pill(k, run.status))
            .children(retry_btn)
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
