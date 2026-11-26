//! A run: its commits, status and duration; a step list; and the open step's log.
//!
//! An act step shows its GitHub Actions substeps as expandable summary cards (click any step to
//! inspect its sliced log) with a switch to view the raw log stream. A step skipped because of
//! `only_on` says so in muted text, since "skipped" alone reads like a problem.

use rune::{col, el, icon, row, text, Cx, El, Icon, ScrollSpec, Spring, Transition, Tx};
use rune_kit::{glyphs, Button, ButtonVariant, ControlSize, Kit};

use crate::model::{Phase, Run, RunStatus, Step, StepKind, StepStatus, Substep};
use crate::state::{LogMode, LogState, State};
use crate::theme;
use crate::widgets::{self, ago, duration, mono_chip, short_sha};

/// Sliced `act` log text associated with each substep, plus any unmatched setup/teardown lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedSubstepLogs {
    pub by_substep: Vec<String>,
    pub preamble: String,
}

enum ActHeader<'a> {
    SetUpJob,
    CompleteJob,
    JobResult,
    StepStart(&'a str),
    StepResult(&'a str),
    Group(&'a str),
}

/// Strips an optional leading `[job]` tag (ignoring `[command]` prefixes).
fn strip_job_prefix(line: &str) -> (Option<&str>, &str) {
    let trimmed = line.trim_start();
    if let Some(after_open) = trimmed.strip_prefix('[') {
        if let Some((tag, rest)) = after_open.split_once(']') {
            if !tag.is_empty()
                && tag != "command"
                && (rest.starts_with(' ') || rest.starts_with('\t'))
            {
                return (Some(tag.trim()), rest.trim_start());
            }
        }
    }
    (None, trimmed)
}

/// Strips leading `act` emojis / status markers and surrounding whitespace.
fn strip_act_marker(s: &str) -> &str {
    let mut rest = s.trim_start();
    loop {
        let before = rest;
        rest = rest.trim_start_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '⭐' | '🐳' | '✅' | '❌' | '🚀' | '⚙' | '☁' | '🏁' | '\u{fe0f}' | '|'
                )
        });
        if rest == before {
            return rest;
        }
    }
}

/// Strips a trailing `[1.8s]` / `[120ms]` duration suffix that `act` appends to result lines.
fn strip_trailing_duration(s: &str) -> &str {
    let trimmed = s.trim();
    if let Some(open) = trimmed.rfind('[') {
        if trimmed.ends_with(']') && open > 0 {
            let inside = &trimmed[open + 1..trimmed.len() - 1];
            if inside
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'm' | 's' | 'h' | 'µ' | 'n'))
            {
                return trimmed[..open].trim_end();
            }
        }
    }
    trimmed
}

fn classify_act_header(body: &str) -> Option<ActHeader<'_>> {
    let cleaned = strip_act_marker(body);
    if cleaned.starts_with("Run Set up job") {
        return Some(ActHeader::SetUpJob);
    }
    if cleaned.starts_with("Run Complete job") {
        return Some(ActHeader::CompleteJob);
    }
    if cleaned.starts_with("Job succeeded") || cleaned.starts_with("Job failed") {
        return Some(ActHeader::JobResult);
    }
    for prefix in ["Run Main ", "Run Pre ", "Run Post "] {
        if let Some(name) = cleaned.strip_prefix(prefix) {
            let name = name.trim();
            if !name.is_empty() {
                return Some(ActHeader::StepStart(name));
            }
        }
    }
    for prefix in [
        "Success - Main ",
        "Failure - Main ",
        "Success - Pre ",
        "Failure - Pre ",
        "Success - Post ",
        "Failure - Post ",
    ] {
        if let Some(name) = cleaned.strip_prefix(prefix) {
            let name = strip_trailing_duration(name);
            if !name.is_empty() {
                return Some(ActHeader::StepResult(name));
            }
        }
    }
    if cleaned.starts_with("Success - Set up job")
        || cleaned.starts_with("Failure - Set up job")
        || cleaned.starts_with("Success - Complete job")
        || cleaned.starts_with("Failure - Complete job")
    {
        return Some(ActHeader::StepResult(if cleaned.contains("Set up job") {
            "Set up job"
        } else {
            "Complete job"
        }));
    }
    if let Some(group) = cleaned.strip_prefix("::group::") {
        let group = group.trim();
        if !group.is_empty() {
            return Some(ActHeader::Group(group));
        }
    }
    None
}

fn substep_name_matches(sub_name: &str, candidate: &str) -> bool {
    let a = sub_name.trim();
    let b = candidate.trim();
    if a == b {
        return true;
    }
    if let Some(stripped) = a.strip_prefix("Run ") {
        if stripped.trim() == b {
            return true;
        }
    }
    if let Some(stripped) = b.strip_prefix("Run ") {
        if a == stripped.trim() {
            return true;
        }
    }
    false
}

fn job_tag_matches(sub: &Substep, job_tag: &str) -> bool {
    if job_tag.is_empty() {
        return true;
    }
    sub.job == job_tag
        || sub.job_id == job_tag
        || sub
            .job
            .rsplit('/')
            .next()
            .is_some_and(|short| short == job_tag)
}

fn match_substep(
    substeps: &[Substep],
    job_tag: Option<&str>,
    name: &str,
    cursor: usize,
) -> Option<usize> {
    if let Some(tag) = job_tag {
        if let Some(idx) = (cursor..substeps.len()).find(|&i| {
            job_tag_matches(&substeps[i], tag) && substep_name_matches(&substeps[i].name, name)
        }) {
            return Some(idx);
        }
        if let Some(idx) = (0..substeps.len()).find(|&i| {
            job_tag_matches(&substeps[i], tag) && substep_name_matches(&substeps[i].name, name)
        }) {
            return Some(idx);
        }
    }
    if let Some(idx) =
        (cursor..substeps.len()).find(|&i| substep_name_matches(&substeps[i].name, name))
    {
        return Some(idx);
    }
    (0..substeps.len()).find(|&i| substep_name_matches(&substeps[i].name, name))
}

fn push_line(buf: &mut String, line: &str) {
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(line);
}

/// Splits `act` log output by step headers and associates lines with each `Substep` in order,
/// preserving any unmatched job setup / teardown lines in `preamble`.
pub fn parse_substep_logs(raw: &str, substeps: &[Substep]) -> ParsedSubstepLogs {
    let mut out = ParsedSubstepLogs {
        by_substep: vec![String::new(); substeps.len()],
        preamble: String::new(),
    };
    if raw.is_empty() {
        return out;
    }

    let mut current: Option<usize> = None;
    let mut cursor: usize = 0;

    for line in raw.lines() {
        let (job_tag, body) = strip_job_prefix(line);
        match classify_act_header(body) {
            Some(ActHeader::SetUpJob) => {
                if let Some(idx) = match_substep(substeps, job_tag, "Set up job", cursor) {
                    current = Some(idx);
                    cursor = idx;
                    push_line(&mut out.by_substep[idx], line);
                } else {
                    current = None;
                    push_line(&mut out.preamble, line);
                }
            }
            Some(ActHeader::CompleteJob) => {
                if let Some(idx) = match_substep(substeps, job_tag, "Complete job", cursor) {
                    current = Some(idx);
                    cursor = idx;
                    push_line(&mut out.by_substep[idx], line);
                } else {
                    current = None;
                    push_line(&mut out.preamble, line);
                }
            }
            Some(ActHeader::JobResult) => {
                current = None;
                push_line(&mut out.preamble, line);
            }
            Some(ActHeader::StepStart(name)) => {
                if let Some(idx) = match_substep(substeps, job_tag, name, cursor) {
                    current = Some(idx);
                    cursor = idx;
                    push_line(&mut out.by_substep[idx], line);
                } else if let Some(cur) = current {
                    // Composite action inner steps (`⭐ Run Main Inner`) stay with their parent step.
                    push_line(&mut out.by_substep[cur], line);
                } else {
                    push_line(&mut out.preamble, line);
                }
            }
            Some(ActHeader::StepResult(name)) => {
                if let Some(idx) = match_substep(substeps, job_tag, name, cursor) {
                    push_line(&mut out.by_substep[idx], line);
                } else if let Some(cur) = current {
                    push_line(&mut out.by_substep[cur], line);
                } else {
                    push_line(&mut out.preamble, line);
                }
            }
            Some(ActHeader::Group(name)) => {
                if let Some(idx) = match_substep(substeps, job_tag, name, cursor) {
                    current = Some(idx);
                    cursor = idx;
                    push_line(&mut out.by_substep[idx], line);
                } else if let Some(cur) = current {
                    push_line(&mut out.by_substep[cur], line);
                } else {
                    push_line(&mut out.preamble, line);
                }
            }
            None => {
                if let Some(cur) = current {
                    push_line(&mut out.by_substep[cur], line);
                } else {
                    push_line(&mut out.preamble, line);
                }
            }
        }
    }

    out
}

pub fn view(
    s: &State,
    k: &Kit,
    id: &str,
    _cx: &mut Cx<State>,
    hair: f32,
    compact: bool,
    layout_w: f32,
) -> El {
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

    let stacked = compact || layout_w < 860.0;
    let body = if stacked {
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(steps_pane(s, k, run, hair, compact, true, layout_w))
            .child(log_pane(s, k, run, hair, compact, layout_w))
    } else {
        row()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(steps_pane(s, k, run, hair, false, false, layout_w))
            .child(log_pane(s, k, run, hair, false, layout_w))
    };

    col()
        .key("run")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header(s, k, run, hair, compact, layout_w))
        .child(body)
}

fn header_shell(k: &Kit, hair: f32, compact: bool) -> El {
    let _ = hair;
    row()
        .key("run-header")
        .w_full()
        .h(52.0)
        .px(if compact { 14.0 } else { 22.0 })
        .center_y()
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
}

fn header(s: &State, k: &Kit, run: &Run, hair: f32, compact: bool, layout_w: f32) -> El {
    let _ = hair;
    let host = s.system.as_ref().map(|x| x.os.clone());
    let repo = crate::views::repo_for(s, &run.project);
    let default_branch = s.current_project().map(|p| p.branch.as_str());
    let ref_badge = widgets::run_ref_badge(k, run, default_branch);
    let deploy_only_pill = run
        .deploy_only
        .then(|| widgets::pill(k, "Deploy only", theme::status(k).running));
    let skipped_notice = run
        .deploy_skipped_reason
        .as_deref()
        .filter(|r| !r.is_empty())
        .map(|reason| {
            row()
                .key("run-deploy-skipped")
                .w_full()
                .gap(6.0)
                .px(10.0)
                .py(6.0)
                .radius(k.radius.sm)
                .fill(k.c.layer_02)
                .border(1.0, k.c.border_muted)
                .center_y()
                .child(widgets::dot(k.c.text_faint, 6.0))
                .child(
                    text(reason.to_string())
                        .font(k.ty.small)
                        .color(k.c.text_muted)
                        .wrap()
                        .min_w(0.0)
                        .grow(1.0),
                )
        });

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
    let cancelling = s.cancelling.as_deref() == Some(run.id.as_str());
    let cancel_id = run.id.clone();
    let cancel_btn = (run.status == RunStatus::Running).then(|| {
        Button::new(if cancelling {
            "Cancelling…"
        } else {
            "Cancel"
        })
        .variant(ButtonVariant::Danger)
        .control_size(ControlSize::Small)
        .disabled(cancelling)
        .help("Stop this running pipeline")
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.cancel_run(&cancel_id, tx))
        .build(k)
        .key("run-cancel")
    });
    let has_deploy_step = run.steps.iter().any(|st| st.phase == Phase::Deploy);
    let project_running = s.current_project().is_some_and(|p| p.running);
    let force_deploy_btn = (!run.status.is_running() && run.pr_number.is_none() && has_deploy_step)
        .then(|| {
            let deploying = s.force_deploying.as_deref() == Some(run.id.as_str());
            let deploy_id = run.id.clone();
            Button::new(if deploying {
                "Deploying…"
            } else {
                "Force deploy"
            })
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .disabled(deploying || project_running)
            .help(if project_running {
                "Already running"
            } else {
                "Queue a deploy-only run for this commit"
            })
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.force_deploy_run(&deploy_id, tx))
            .build(k)
            .key("run-force-deploy")
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
        let facts = row()
            .w_full()
            .gap(8.0)
            .flex_wrap()
            .center_y()
            .child(widgets::run_pill(k, run.status))
            .children(ref_badge)
            .children(deploy_only_pill)
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

        let mut actions = row().w_full().gap(8.0).flex_wrap();
        if let Some(btn) = cancel_btn {
            actions = actions.child(btn.grow(1.0));
        }
        if let Some(btn) = force_deploy_btn {
            actions = actions.child(btn.grow(1.0));
        }
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
            .fill(k.c.layer_01)
            .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
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
            .children(skipped_notice)
            .child(actions)
            .children(
                (run.status == RunStatus::Running)
                    .then(|| rune_kit::Progress::new(None).build(k).key("run-progress")),
            );
    }

    let mut meta = row().w_full().gap(8.0).center_y();
    if layout_w < 960.0 {
        meta = meta.flex_wrap();
    }
    meta = meta
        .child(widgets::run_pill(k, run.status))
        .children(ref_badge)
        .children(deploy_only_pill)
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

    if let Some(btn) = cancel_btn {
        meta = meta.child(btn);
    }
    if let Some(btn) = force_deploy_btn {
        meta = meta.child(btn);
    }
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
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
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
        .children(skipped_notice)
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

/// The step list, on the left on wide desktop or stacked above the log on compact / narrow desktop.
fn steps_pane(
    s: &State,
    k: &Kit,
    run: &Run,
    hair: f32,
    compact: bool,
    stacked: bool,
    layout_w: f32,
) -> El {
    let _ = hair;
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

    if stacked {
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
            .pb(theme::SECTION_BORDER)
            .fill(k.c.layer_01)
            .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
            .child(widgets::scroll_bar(scroller, k));
        if s.open_step.is_none() {
            pane = pane.grow(1.0).min_h(0.0);
        }
        return pane;
    }

    let steps_w = if layout_w < 1080.0 { 230.0 } else { 300.0 };
    col()
        .key("run-steps")
        .w(steps_w)
        .shrink()
        .h_full()
        .min_h(0.0)
        .pr(theme::SECTION_BORDER)
        .fill(k.c.layer_01)
        .border_sides([0.0, theme::SECTION_BORDER, 0.0, 0.0], k.c.border_strong)
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
        .fill(if selected {
            k.c.layer_03
        } else {
            rune::Color::TRANSPARENT
        })
        .hover(if selected { k.c.layer_03 } else { k.c.hover })
        .press_scale(0.992)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
            if s.open_step == Some(index) {
                s.open_step = None;
                s.stick_log_pending = false;
                s.sync_ws_subscriptions();
            } else {
                s.open_step = Some(index);
                s.shell_log_collapsed = false;
                let entry = s.logs.get(&(run_id.clone(), index));
                let mode = entry.and_then(|e| e.mode).unwrap_or(LogMode::Tail);
                let raw_scroll = !has_substeps || s.log_view_raw;
                s.stick_log_pending = raw_scroll && entry.is_none_or(|e| e.text.is_empty());
                s.request_log(&run_id, index, mode);
                if raw_scroll {
                    tx.scroll_to_bottom_instant("run-log-scroll");
                }
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

/// The log pane: expandable summary steps (default for act steps) or raw streaming log.
fn log_pane(s: &State, k: &Kit, run: &Run, hair: f32, compact: bool, layout_w: f32) -> El {
    let _ = hair;
    let Some(index) = s.open_step else {
        if compact {
            return el().key("run-log-none").h(0.0);
        }
        return col()
            .key("run-log")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .min_w(0.0)
            .h_full()
            .fill(k.c.bg_deep)
            .child(widgets::empty_state(
                k,
                "Pick a step",
                "Its output appears here. Act steps show expandable GitHub Actions steps.",
            ));
    };
    let Some(step) = run.steps.get(index) else {
        return widgets::empty_state(k, "Pick a step", "Its output appears here.");
    };
    let host = s.system.as_ref().map(|x| x.os.clone());
    let only_on = effective_only_on(s, run, step);
    let state = s.logs.get(&(run.id.clone(), index));
    let has_substeps = !step.substeps.is_empty();
    let show_steps = has_substeps && !s.log_view_raw;

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
                let raw_scroll = !has_substeps || s.log_view_raw;
                s.stick_log_pending = raw_scroll;
                s.request_log(&run_id, index, mode);
                if raw_scroll {
                    tx.scroll_to_bottom_instant("run-log-scroll");
                }
                s.refresh(tx);
            }
        },
    )
    .key("run-log-mode");

    let view_ctrl = has_substeps.then(|| {
        widgets::segmented(
            k,
            compact,
            &["Steps", "Raw"],
            Some(if s.log_view_raw { 1 } else { 0 }),
            |s: &mut State, picked: usize, tx: &mut Tx| {
                let want_raw = picked == 1;
                if s.log_view_raw != want_raw {
                    s.log_view_raw = want_raw;
                    if want_raw {
                        s.stick_log_pending = true;
                        tx.scroll_to_bottom_instant("run-log-scroll");
                    } else {
                        s.stick_log_pending = false;
                    }
                }
            },
        )
        .key("run-log-view")
    });

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

    let narrow_head = compact || layout_w < 1000.0;
    let mut controls = row().gap(6.0).center_y();
    if narrow_head {
        controls = controls.flex_wrap();
    }
    if let Some(vc) = view_ctrl {
        controls = controls.child(vc);
    }
    controls = controls.child(wrap_btn).child(mode_ctrl);

    let mut head = row()
        .w_full()
        .px(if compact { 14.0 } else { 16.0 })
        .py(9.0)
        .gap(10.0)
        .center_y()
        .fill(k.c.layer_02)
        .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong);
    if narrow_head {
        head = head.flex_wrap();
    }
    head = head
        .child(widgets::status_glyph(k, step.status, host.as_deref()))
        .child(title_block)
        .child(controls);

    let log_scroll = if show_steps {
        let accordion = substeps_accordion(s, k, run, index, step, state, host.as_deref(), compact);
        let viewport = col()
            .key("run-log-scroll")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .min_w(0.0)
            .scroll(ScrollSpec::default())
            .child(accordion);
        widgets::scroll_bar(viewport, k)
    } else {
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
        body = body.child(log_block(
            s,
            k,
            step,
            state,
            s.log_wrap,
            compact,
            !has_substeps,
        ));

        let log_viewport = col()
            .key("run-log-scroll")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .scroll(ScrollSpec::stick_bottom());
        if s.log_wrap {
            widgets::scroll_bar(log_viewport.child(body), k)
        } else {
            widgets::scroll_bars_xy(log_viewport.scroll_xy().child(body), k)
        }
    };

    col()
        .key("run-log")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .min_w(0.0)
        .h_full()
        .fill(k.c.bg_deep)
        .child(head)
        .child(log_scroll)
}

/// Expandable GitHub Actions summary steps accordion (`Checkout`, `Validate`, `Build` -> click to expand).
#[allow(clippy::too_many_arguments)]
fn substeps_accordion(
    s: &State,
    k: &Kit,
    run: &Run,
    step_index: usize,
    step: &Step,
    state: Option<&LogState>,
    host: Option<&str>,
    compact: bool,
) -> El {
    let raw = state.map(|e| e.text.as_str()).unwrap_or("");
    let loading = state.is_none_or(|e| e.loading);
    let parsed = parse_substep_logs(raw, &step.substeps);
    let has_preamble = !parsed.preamble.trim().is_empty();
    let any_matched = parsed.by_substep.iter().any(|b| !b.trim().is_empty());
    let preamble_auto_open = has_preamble && !any_matched;
    let total_cards = step.substeps.len() + usize::from(has_preamble);

    let all_expanded = (0..step.substeps.len())
        .all(|i| s.is_substep_expanded(&run.id, step_index, i, step.substeps[i].status))
        && (!has_preamble || {
            let step_key = (run.id.clone(), step_index);
            if s.substep_toggled.contains(&step_key) {
                s.expanded_substeps
                    .contains(&(run.id.clone(), step_index, step.substeps.len()))
            } else {
                preamble_auto_open
            }
        });

    let toggle_all_btn = Button::new(if all_expanded {
        "Collapse all"
    } else {
        "Expand all"
    })
    .variant(ButtonVariant::Ghost)
    .control_size(ControlSize::Small)
    .on_press_tx({
        let run_id = run.id.clone();
        let want_expand = !all_expanded;
        move |s: &mut State, _tx: &mut Tx| {
            s.set_all_substeps_expanded(&run_id, step_index, total_cards, want_expand);
        }
    })
    .build(k)
    .key("substeps-expand-all");

    let mut jobs: Vec<(&str, Vec<(usize, &Substep)>)> = Vec::new();
    for (sub_idx, sub) in step.substeps.iter().enumerate() {
        match jobs.iter_mut().find(|(job, _)| *job == sub.job.as_str()) {
            Some((_, steps)) => steps.push((sub_idx, sub)),
            None => jobs.push((sub.job.as_str(), vec![(sub_idx, sub)])),
        }
    }

    let mut block = col()
        .key("substeps")
        .w_full()
        .min_w(0.0)
        .pl(if compact { 14.0 } else { 16.0 })
        .pr(18.0)
        .py(12.0)
        .gap(10.0)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(widgets::caption(k, "GITHUB ACTIONS"))
                .child(el().grow(1.0))
                .child(toggle_all_btn),
        );

    if let Some(err) = state
        .and_then(|e| e.error.as_deref())
        .filter(|e| !e.is_empty())
    {
        block = block.child(widgets::notice(k, theme::status(k).failure, err));
    }

    for (job_idx, (job, steps)) in jobs.into_iter().enumerate() {
        let mut job_block = col()
            .key(format!("substeps-job-{job_idx}"))
            .w_full()
            .min_w(0.0)
            .gap(6.0);
        if !job.is_empty() {
            let job_txt = text(job.to_string())
                .font(k.ty.small_medium)
                .color(k.c.text_muted)
                .min_w(0.0);
            job_block = job_block.child(
                row()
                    .w_full()
                    .gap(6.0)
                    .pt(if job_idx > 0 { 6.0 } else { 0.0 })
                    .center_y()
                    .child(if compact {
                        job_txt.wrap_anywhere().shrink()
                    } else {
                        job_txt.wrap().shrink()
                    }),
            );
        }
        for (sub_idx, sub) in steps {
            let expanded = s.is_substep_expanded(&run.id, step_index, sub_idx, sub.status);
            let slice = parsed
                .by_substep
                .get(sub_idx)
                .map(String::as_str)
                .unwrap_or("");
            job_block = job_block.child(substep_card(
                s,
                k,
                run,
                step_index,
                step,
                sub_idx,
                sub,
                slice,
                loading,
                expanded,
                preamble_auto_open,
                host,
                compact,
            ));
        }
        block = block.child(job_block);
    }

    if has_preamble {
        let preamble_idx = step.substeps.len();
        let step_key = (run.id.clone(), step_index);
        let expanded = if s.substep_toggled.contains(&step_key) {
            s.expanded_substeps
                .contains(&(run.id.clone(), step_index, preamble_idx))
        } else {
            preamble_auto_open
        };
        let run_id = run.id.clone();
        let substeps_clone = step.substeps.clone();
        let chevron = icon(
            if expanded {
                &glyphs::TRIANGLE_DOWN
            } else {
                &glyphs::TRIANGLE_RIGHT
            },
            12.0,
        )
        .color(k.c.icon_muted);

        let header = row()
            .key("substep-toggle-preamble")
            .role("button")
            .label("Job setup / teardown")
            .a11y_value(if expanded { "expanded" } else { "collapsed" })
            .w_full()
            .min_h(if compact { 44.0 } else { 34.0 })
            .px(10.0)
            .py(6.0)
            .gap(8.0)
            .center_y()
            .hover(k.c.layer_03)
            .press_scale(0.992)
            .cursor(rune::Cursor::Pointer)
            .on_press_tx(move |s: &mut State, _tx: &mut Tx| {
                s.toggle_substep(
                    &run_id,
                    step_index,
                    preamble_idx,
                    &substeps_clone,
                    preamble_auto_open,
                );
            })
            .child(chevron)
            .child(widgets::dot(k.c.icon_faint, 6.0))
            .child(
                text("Job setup / teardown")
                    .font(k.ty.small_medium)
                    .color(k.c.text_muted)
                    .min_w(0.0)
                    .grow(1.0),
            );

        let mut card = col()
            .key("substep-card-preamble")
            .w_full()
            .min_w(0.0)
            .gap(0.0)
            .radius(k.radius.md)
            .fill(k.c.layer_02)
            .border(1.0, k.c.border_base)
            .border_layout()
            .clip_rounded(k.radius.md)
            .child(header);
        if expanded {
            card = card.child(expanded_log_box(
                k,
                "substep-log-preamble",
                &parsed.preamble,
                false,
                s.log_wrap,
            ));
        }
        block = block.child(card);
    }

    block
}

#[allow(clippy::too_many_arguments)]
fn substep_card(
    s: &State,
    k: &Kit,
    run: &Run,
    step_index: usize,
    step: &Step,
    sub_idx: usize,
    sub: &Substep,
    slice: &str,
    loading: bool,
    expanded: bool,
    preamble_auto_open: bool,
    host: Option<&str>,
    compact: bool,
) -> El {
    let ink = theme::status(k).for_step(sub.status);
    let dur_text = sub.duration_ms.map(widgets::duration).unwrap_or_default();
    let run_id = run.id.clone();
    let substeps_clone = step.substeps.clone();

    let name_el = text(sub.name.clone())
        .font(k.ty.small_medium)
        .color(k.c.text_base)
        .min_w(0.0);

    let chevron = icon(
        if expanded {
            &glyphs::TRIANGLE_DOWN
        } else {
            &glyphs::TRIANGLE_RIGHT
        },
        12.0,
    )
    .color(k.c.icon_muted);

    let header = row()
        .key(format!("substep-toggle-{sub_idx}"))
        .role("button")
        .label(sub.name.clone())
        .a11y_value(if expanded { "expanded" } else { "collapsed" })
        .w_full()
        .min_h(if compact { 44.0 } else { 34.0 })
        .px(10.0)
        .py(6.0)
        .gap(8.0)
        .center_y()
        .hover(k.c.layer_03)
        .press_scale(0.992)
        .cursor(rune::Cursor::Pointer)
        .on_press_tx(move |s: &mut State, _tx: &mut Tx| {
            s.toggle_substep(
                &run_id,
                step_index,
                sub_idx,
                &substeps_clone,
                preamble_auto_open,
            );
        })
        .child(chevron)
        .child(widgets::status_glyph(k, sub.status, host))
        .child(if compact {
            name_el.wrap().grow(1.0)
        } else {
            name_el.nowrap().truncate().grow(1.0)
        })
        .children((!dur_text.is_empty()).then(|| {
            text(dur_text)
                .font(k.ty.small)
                .color(k.c.text_faint)
                .nowrap()
        }))
        .children((sub.status == StepStatus::Running).then(|| widgets::pill(k, "running", ink)));

    let mut card = col()
        .key(format!("substep-card-{sub_idx}"))
        .w_full()
        .min_w(0.0)
        .gap(0.0)
        .radius(k.radius.md)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base)
        .border_layout()
        .clip_rounded(k.radius.md)
        .child(header);

    if expanded {
        card = card.child(expanded_log_box(
            k,
            &format!("substep-log-{sub_idx}"),
            slice,
            loading,
            s.log_wrap,
        ));
    }
    card
}

fn expanded_log_box(k: &Kit, key: &str, slice: &str, loading: bool, wrap: bool) -> El {
    let content = if slice.is_empty() {
        col().w_full().px(12.0).py(10.0).child(
            text(if loading {
                "Reading…"
            } else {
                "No output recorded for this step."
            })
            .font(k.ty.small)
            .color(k.c.text_faint),
        )
    } else if wrap {
        widgets::log_body(k, slice, true)
    } else {
        widgets::scroll_bar_x(
            col()
                .w_full()
                .min_w(0.0)
                .scroll_x()
                .child(widgets::log_body(k, slice, false)),
            k,
        )
    };

    col()
        .key(key)
        .w_full()
        .min_w(0.0)
        .fill(k.c.bg_deep)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_base)
        .transition(Transition::asymmetric(
            Transition::opacity(),
            Transition::none(),
        ))
        .transition_motion(rune::tok::motion::SNAP, Spring::NONE)
        .child(content)
}

fn log_block(
    s: &State,
    k: &Kit,
    step: &Step,
    state: Option<&LogState>,
    wrap: bool,
    compact: bool,
    collapsible_shell: bool,
) -> El {
    let mut block = col()
        .key("log")
        .w_full()
        .pl(16.0)
        .pr(18.0)
        .pt(10.0)
        .pb(if wrap { 16.0 } else { 18.0 })
        .gap(6.0);
    if wrap {
        block = block.min_w(0.0);
    }

    if collapsible_shell {
        let collapsed = s.shell_log_collapsed;
        let line_count = state.map(|e| e.text.lines().count()).unwrap_or(0);
        let summary = if line_count > 0 {
            format!(
                "{line_count} {}",
                if line_count == 1 { "line" } else { "lines" }
            )
        } else {
            String::new()
        };
        block = block.child(
            row()
                .key("shell-log-toggle")
                .role("button")
                .label("Output")
                .a11y_value(if collapsed { "collapsed" } else { "expanded" })
                .w_full()
                .min_h(if compact { 44.0 } else { 26.0 })
                .px(8.0)
                .gap(6.0)
                .center_y()
                .radius(k.radius.sm)
                .hover(k.c.layer_02)
                .cursor(rune::Cursor::Pointer)
                .on_press(|s: &mut State| {
                    s.shell_log_collapsed = !s.shell_log_collapsed;
                })
                .child(
                    icon(
                        if collapsed {
                            &glyphs::TRIANGLE_RIGHT
                        } else {
                            &glyphs::TRIANGLE_DOWN
                        },
                        12.0,
                    )
                    .color(k.c.icon_muted),
                )
                .child(widgets::caption(k, "OUTPUT"))
                .child(el().grow(1.0))
                .children((!summary.is_empty()).then(|| {
                    text(summary)
                        .font(k.ty.small)
                        .color(k.c.text_faint)
                        .nowrap()
                })),
        );
        if collapsed {
            return block;
        }
    } else {
        block = block.child(widgets::caption(k, "OUTPUT"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(job: &str, job_id: &str, name: &str, status: StepStatus) -> Substep {
        Substep {
            job: job.into(),
            job_id: job_id.into(),
            name: name.into(),
            status,
            duration_ms: Some(1000),
        }
    }

    #[test]
    fn parse_substep_logs_slices_checkout_validate_build_and_preamble() {
        let substeps = vec![
            sub("Web", "web", "Checkout", StepStatus::Success),
            sub("Web", "web", "Validate", StepStatus::Success),
            sub("Web", "web", "Build", StepStatus::Failure),
        ];
        let raw = "\
🚀  Start image=catthehacker/ubuntu:act-latest
  🐳  docker create image=catthehacker/ubuntu:act-latest
⭐ Run Set up job
  ✅  Success - Set up job
⭐ Run Main Checkout
  🐳  docker cp src=. -> /workspace
  ✅  Success - Main Checkout [1.8s]
⭐ Run Main Validate
cargo fmt --check
cargo clippy --all-targets
  ✅  Success - Main Validate [12.4s]
⭐ Run Main Build
compiling crate lunacy...
error[E0308]: mismatched types
  ❌  Failure - Main Build [3.2s]
exit status 1
⭐ Run Post Checkout
cleaning credentials
⭐ Run Complete job
🏁  Job failed";

        let parsed = parse_substep_logs(raw, &substeps);
        assert!(parsed.preamble.contains("Start image="));
        assert!(parsed.preamble.contains("Run Set up job"));
        assert!(parsed.preamble.contains("Run Complete job"));
        assert!(parsed.preamble.contains("Job failed"));

        assert!(parsed.by_substep[0].contains("Run Main Checkout"));
        assert!(parsed.by_substep[0].contains("docker cp"));
        assert!(parsed.by_substep[0].contains("Success - Main Checkout"));
        assert!(parsed.by_substep[0].contains("Run Post Checkout"));

        assert!(parsed.by_substep[1].contains("cargo fmt --check"));
        assert!(parsed.by_substep[1].contains("cargo clippy"));
        assert!(!parsed.by_substep[1].contains("compiling crate"));

        assert!(parsed.by_substep[2].contains("compiling crate lunacy"));
        assert!(parsed.by_substep[2].contains("mismatched types"));
        assert!(parsed.by_substep[2].contains("Failure - Main Build"));
        assert!(parsed.by_substep[2].contains("exit status 1"));
    }

    #[test]
    fn parse_substep_logs_handles_multi_job_prefixes_and_composite_steps() {
        let substeps = vec![
            sub("Web", "web", "Checkout", StepStatus::Success),
            sub(
                "Web",
                "web",
                "Format, analyze, test and build Web in parallel",
                StepStatus::Success,
            ),
            sub("Server", "server", "Checkout", StepStatus::Success),
            sub("Server", "server", "Test", StepStatus::Success),
        ];
        let raw = "\
[web] ⭐ Run Main Checkout
[web] checked out web
[web] ⭐ Run Main Format, analyze, test and build Web in parallel
[web] ⭐ Run Main Inner composite action
[web] inner output line
[web]   ✅  Success - Main Format, analyze, test and build Web in parallel
[server] ⭐ Run Main Checkout
[server] checked out server
[server] ⭐ Run Main Test
[server] test result: ok. 62 passed";

        let parsed = parse_substep_logs(raw, &substeps);
        assert!(parsed.by_substep[0].contains("checked out web"));
        assert!(!parsed.by_substep[0].contains("checked out server"));
        assert!(parsed.by_substep[1].contains("Inner composite action"));
        assert!(parsed.by_substep[1].contains("inner output line"));
        assert!(parsed.by_substep[2].contains("checked out server"));
        assert!(parsed.by_substep[3].contains("62 passed"));
    }
}
