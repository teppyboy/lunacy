//! Deploy settings inspection: pre-flight checks, checkout safety, timeouts, reporting,
//! deploy step readiness, and last deploy status from `GET /api/deploy/checks`.

use rune::{col, el, icon, row, text, Cx, El, Tx};
use rune_kit::{glyphs, Button, ButtonVariant, ControlSize, Kit};

use crate::model::{CheckItem, CheckLevel, DeployProjectCheck, DeployStepCheck, ProjectState};
use crate::state::State;
use crate::theme;
use crate::widgets::{self, duration, mono_chip, short_sha};

pub fn view(s: &State, k: &Kit, _cx: &mut Cx<State>, compact: bool) -> El {
    let mut c = col()
        .key("deploy-checks")
        .w_full()
        .pl(if compact { 14.0 } else { 22.0 })
        .pr(if compact { 18.0 } else { 22.0 })
        .py(if compact { 14.0 } else { 18.0 })
        .gap(18.0);

    let recheck_btn = Button::new(if s.deploy_checks_loading {
        "Checking…"
    } else {
        "Re-check"
    })
    .variant(ButtonVariant::Outline)
    .control_size(ControlSize::Small)
    .disabled(s.deploy_checks_loading)
    .on_press_tx(|s: &mut State, tx: &mut Tx| {
        s.request_deploy_checks();
        s.refresh(tx);
    })
    .build(k)
    .key("deploy-refresh");

    let mut head = row().w_full().gap(10.0).center_y();
    if compact {
        head = head.flex_wrap();
    }
    head = head
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .gap(2.0)
                .child(
                    text("Deploy settings")
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base),
                )
                .child(
                    text(
                        "Pre-flight checks, checkout safety, timeouts, and deploy step readiness.",
                    )
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .wrap(),
                ),
        )
        .child(recheck_btn);
    c = c.child(head);

    if let Some(error) = &s.deploy_checks_error {
        c = c.child(widgets::notice(k, theme::status(k).failure, error));
    }

    let Some(report) = &s.deploy_checks else {
        return c.child(widgets::empty_state(
            k,
            "Checking deploy readiness…",
            "`GET /api/deploy/checks` has not answered yet.",
        ));
    };

    // Host overview card
    let gh_label = if report.github_enabled {
        match report.github_mode.as_deref() {
            Some(mode) if !mode.is_empty() => format!("Enabled ({mode})"),
            _ => "Enabled".into(),
        }
    } else {
        "Disabled".into()
    };
    let gh_ink = if report.github_enabled {
        theme::status(k).success
    } else {
        k.c.text_faint
    };
    let discord_pill = if report.discord_global {
        widgets::pill(k, "Configured", theme::status(k).success)
    } else {
        widgets::pill(k, "Not configured", k.c.text_faint)
    };

    let cache_row_val = {
        let mut r = row()
            .gap(6.0)
            .center_y()
            .flex_wrap()
            .child(if report.cache_enabled {
                widgets::pill(k, "Enabled", theme::status(k).success)
            } else {
                widgets::pill(k, "Disabled", k.c.text_faint)
            });
        if !report.cache_dir.is_empty() {
            r = r.child(
                text(report.cache_dir.clone())
                    .font(k.ty.mono)
                    .color(k.c.text_muted)
                    .wrap_anywhere()
                    .min_w(0.0),
            );
        }
        r
    };

    let host_card = col()
        .key("deploy-host-overview")
        .w_full()
        .padding(14.0)
        .gap(8.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base)
        .child(widgets::caption(k, "HOST & DEFAULTS"))
        .child(widgets::kv_for(
            k,
            compact,
            "Host OS",
            row()
                .gap(6.0)
                .center_y()
                .flex_wrap()
                .child(
                    text(widgets::os_name(&report.os))
                        .font(k.ty.small_medium)
                        .color(k.c.text_base),
                )
                .child(widgets::pill(k, report.os.clone(), k.c.text_accent)),
        ))
        .child(widgets::divider(k))
        .child(widgets::kv_for(
            k,
            compact,
            "Default step timeout",
            widgets::value_for(k, compact, &format!("{}s", report.default_step_timeout)),
        ))
        .child(widgets::divider(k))
        .child(widgets::kv_for(
            k,
            compact,
            "GitHub reporting",
            widgets::pill(k, gh_label, gh_ink),
        ))
        .child(widgets::divider(k))
        .child(widgets::kv_for(k, compact, "Global Discord", discord_pill))
        .child(widgets::divider(k))
        .child(widgets::kv_for(k, compact, "Build cache", cache_row_val));
    c = c.child(host_card);

    if report.projects.is_empty() {
        c = c.child(widgets::notice(
            k,
            k.c.text_faint,
            "No projects configured yet. Add a project in Settings › Projects.",
        ));
        return c;
    }

    for (idx, proj) in report.projects.iter().enumerate() {
        c = c.child(project_card(s, k, &report.os, idx, proj, compact));
    }
    c
}

fn overall_pill(k: &Kit, level: CheckLevel) -> El {
    let st = theme::status(k);
    let (label, ink) = match level {
        CheckLevel::Ok => ("Ready", st.success),
        CheckLevel::Warn => ("Warnings", st.error),
        CheckLevel::Error => ("Issues", st.failure),
        CheckLevel::Other => ("Unknown", st.pending),
    };
    widgets::pill(k, label, ink)
}

fn check_ink(k: &Kit, level: CheckLevel) -> rune::Color {
    let st = theme::status(k);
    match level {
        CheckLevel::Ok => st.success,
        CheckLevel::Warn => st.error,
        CheckLevel::Error => st.failure,
        CheckLevel::Other => st.pending,
    }
}

fn check_row(k: &Kit, item: &CheckItem, compact: bool) -> El {
    let ink = check_ink(k, item.level);
    let marker = match item.level {
        CheckLevel::Ok => icon(&glyphs::CHECK_SMALL, 13.0).color(ink).mt(2.0),
        CheckLevel::Warn => widgets::dot(ink, 7.0).mt(5.0),
        CheckLevel::Error => icon(&glyphs::CIRCLE_X, 13.0).color(ink).mt(2.0),
        CheckLevel::Other => widgets::dot(ink, 7.0).mt(5.0),
    };
    let detail_el = text(item.detail.clone())
        .font(k.ty.small)
        .color(k.c.text_muted)
        .min_w(0.0);
    row().w_full().gap(8.0).items_start().child(marker).child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(1.0)
            .child(
                text(item.label.clone())
                    .font(k.ty.small_medium)
                    .color(k.c.text_base)
                    .wrap(),
            )
            .children((!item.detail.is_empty()).then(|| {
                if compact {
                    detail_el.wrap_anywhere()
                } else {
                    detail_el.wrap()
                }
            })),
    )
}

fn project_config_index(
    editor: &crate::state::Editor,
    name: &str,
    fallback_idx: usize,
) -> Option<usize> {
    if editor.loaded {
        crate::paths::array(&editor.draft, "project")
            .iter()
            .position(|p| p.get("name").and_then(|n| n.as_str()) == Some(name))
    } else {
        Some(fallback_idx)
    }
}

fn project_card(
    s: &State,
    k: &Kit,
    host_os: &str,
    idx: usize,
    proj: &DeployProjectCheck,
    compact: bool,
) -> El {
    let config_idx = project_config_index(&s.editor, &proj.name, idx);

    let edit_btn = Button::new("Edit")
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_project_form(config_idx, tx))
        .build(k)
        .key(format!("deploy-edit-{}", proj.name));

    let last_run_btn = proj.last_deploy.as_ref().map(|last| {
        let run_id = last.run_id.clone();
        Button::new("Open last run")
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_run(&run_id, tx))
            .build(k)
            .key(format!("deploy-last-run-{}", proj.name))
    });

    let busy = s
        .projects
        .iter()
        .any(|p| p.name == proj.name && p.state() == ProjectState::Running);
    let deploying = s.force_deploying.as_deref() == Some(proj.name.as_str());
    let force_deploy_btn = (proj.deploy_total_steps > 0).then(|| {
        let deploy_name = proj.name.clone();
        Button::new(if deploying {
            "Deploying…"
        } else {
            "Force deploy"
        })
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .disabled(deploying || busy)
        .help(if busy {
            "Already running"
        } else {
            "Queue a deploy-only run for the current commit"
        })
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.force_deploy_project(&deploy_name, tx))
        .build(k)
        .key(format!("deploy-force-{}", proj.name))
    });

    let mut header = row().w_full().gap(8.0).center_y();
    if compact {
        header = header.flex_wrap();
    }
    header = header
        .child(
            text(proj.name.clone())
                .font(k.ty.ui_medium)
                .color(k.c.text_base)
                .wrap()
                .min_w(0.0),
        )
        .child(overall_pill(k, proj.overall));
    if !compact {
        header = header.child(el().grow(1.0));
    }
    if let Some(btn) = force_deploy_btn {
        header = header.child(btn);
    }
    if let Some(btn) = last_run_btn {
        header = header.child(btn);
    }
    header = header.child(edit_btn);

    let checkout_val = {
        let mut r = row().gap(6.0).center_y().flex_wrap().child(
            text(proj.checkout.clone())
                .font(k.ty.mono)
                .color(k.c.text_base),
        );
        if proj.git.dirty == Some(true) {
            r = r.child(widgets::pill(k, "Dirty worktree", theme::status(k).error));
        }
        r
    };

    let timeouts_str = match proj.pipeline_timeout {
        Some(total) => format!("step {}s · pipeline {}s", proj.step_timeout, total),
        None => format!("step {}s · no pipeline cap", proj.step_timeout),
    };

    let reporting_str = {
        let gh = if proj.reporting.github_enabled {
            let ctx = if proj.reporting.deploy_context.is_empty() {
                "lunacy/deploy"
            } else {
                proj.reporting.deploy_context.as_str()
            };
            format!("GitHub: {ctx}")
        } else {
            "GitHub: off".to_string()
        };
        let dc = if proj.reporting.discord_source.is_empty() {
            "none"
        } else {
            proj.reporting.discord_source.as_str()
        };
        format!("{gh} · Discord: {dc}")
    };

    let steps_str = format!(
        "{}/{} CI · {}/{} deploy on {}",
        proj.ci_active_steps,
        proj.ci_total_steps,
        proj.deploy_active_steps,
        proj.deploy_total_steps,
        widgets::os_name(host_os)
    );

    let deploy_refs_str = if proj.deploy_refs.is_empty() {
        format!("{} (default branch)", proj.branch)
    } else {
        proj.deploy_refs.join(", ")
    };

    let mut card = col()
        .key(format!("deploy-project-{}", proj.name))
        .w_full()
        .padding(14.0)
        .gap(12.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base)
        .child(header)
        .child(widgets::divider(k))
        .child(
            col()
                .w_full()
                .gap(0.0)
                .child(widgets::kv_for(
                    k,
                    compact,
                    "Branch & remote",
                    widgets::value_for(k, compact, &format!("{} / {}", proj.remote, proj.branch)),
                ))
                .child(widgets::divider(k))
                .child(widgets::kv_for(k, compact, "Checkout mode", checkout_val))
                .children(
                    proj.trigger_mode
                        .as_deref()
                        .filter(|m| !m.is_empty())
                        .map(|mode| {
                            col().w_full().gap(0.0).child(widgets::divider(k)).child(
                                widgets::kv_for(
                                    k,
                                    compact,
                                    "Trigger mode",
                                    widgets::pill(
                                        k,
                                        mode,
                                        if mode == "webhook" {
                                            theme::status(k).success
                                        } else {
                                            k.c.text_accent
                                        },
                                    ),
                                ),
                            )
                        }),
                )
                .child(widgets::divider(k))
                .child(widgets::kv_for(
                    k,
                    compact,
                    "PR builds",
                    if proj.watch_prs {
                        widgets::pill(k, "Enabled (CI only)", k.c.text_accent)
                    } else {
                        widgets::pill(k, "Disabled", k.c.text_faint)
                    },
                ))
                .child(widgets::divider(k))
                .child(widgets::kv_for(
                    k,
                    compact,
                    "Deploy refs",
                    text(deploy_refs_str)
                        .font(k.ty.mono)
                        .color(k.c.text_base)
                        .wrap_anywhere()
                        .min_w(0.0),
                ))
                .child(widgets::divider(k))
                .child(widgets::kv_for(
                    k,
                    compact,
                    "Build cache",
                    if proj.cache_enabled {
                        widgets::pill(k, "Enabled", theme::status(k).success)
                    } else {
                        widgets::pill(k, "Disabled", k.c.text_faint)
                    },
                ))
                .child(widgets::divider(k))
                .child(widgets::kv_for(
                    k,
                    compact,
                    "Repository path",
                    text(proj.repo_path.clone())
                        .font(k.ty.mono)
                        .color(k.c.text_base)
                        .wrap_anywhere()
                        .min_w(0.0),
                ))
                .child(widgets::divider(k))
                .child(widgets::kv_for(
                    k,
                    compact,
                    "Timeouts",
                    text(timeouts_str)
                        .font(k.ty.small)
                        .color(k.c.text_base)
                        .wrap()
                        .min_w(0.0),
                ))
                .child(widgets::divider(k))
                .child(widgets::kv_for(
                    k,
                    compact,
                    "Reporting",
                    text(reporting_str)
                        .font(k.ty.small)
                        .color(k.c.text_base)
                        .wrap()
                        .min_w(0.0),
                ))
                .child(widgets::divider(k))
                .child(widgets::kv_for(
                    k,
                    compact,
                    "Active steps",
                    text(steps_str)
                        .font(k.ty.small)
                        .color(k.c.text_base)
                        .wrap()
                        .min_w(0.0),
                )),
        );

    if !proj.checks.is_empty() {
        let mut checks_col = col()
            .w_full()
            .gap(6.0)
            .child(widgets::caption(k, "PRE-FLIGHT CHECKS"));
        for item in &proj.checks {
            checks_col = checks_col.child(check_row(k, item, compact));
        }
        card = card.child(widgets::divider(k)).child(checks_col);
    }

    let mut steps_col = col()
        .w_full()
        .gap(8.0)
        .child(widgets::caption(k, "DEPLOY STEPS"));
    if proj.deploy_steps.is_empty() {
        steps_col = steps_col.child(
            text("No deploy steps configured for this project.")
                .font(k.ty.small)
                .color(k.c.text_faint)
                .wrap(),
        );
    } else {
        for (i, step) in proj.deploy_steps.iter().enumerate() {
            steps_col = steps_col.child(deploy_step_card(k, host_os, i, step, compact));
        }
    }
    card = card.child(widgets::divider(k)).child(steps_col);

    if let Some(last) = &proj.last_deploy {
        let mut last_col = col()
            .w_full()
            .gap(8.0)
            .child(widgets::caption(k, "LAST DEPLOY"));
        let mut meta = row()
            .w_full()
            .gap(8.0)
            .flex_wrap()
            .center_y()
            .child(mono_chip(k, &short_sha(&last.sha)))
            .child(widgets::run_pill(k, last.status));
        if let Some(ts) = last.finished_at {
            meta = meta.child(
                text(widgets::timestamp(ts))
                    .font(k.ty.small)
                    .color(k.c.text_faint)
                    .nowrap(),
            );
        }
        last_col = last_col.child(meta);
        for step in &last.steps {
            let dur = step.duration_ms.map(duration).unwrap_or_default();
            last_col = last_col.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(widgets::status_glyph(k, step.status, Some(host_os)))
                    .child(
                        text(step.name.clone())
                            .font(k.ty.small)
                            .color(k.c.text_base)
                            .wrap()
                            .min_w(0.0)
                            .grow(1.0),
                    )
                    .children(
                        (!dur.is_empty())
                            .then(|| text(dur).font(k.ty.small).color(k.c.text_faint).nowrap()),
                    ),
            );
        }
        card = card.child(widgets::divider(k)).child(last_col);
    }

    card
}

fn deploy_step_card(
    k: &Kit,
    host_os: &str,
    idx: usize,
    step: &DeployStepCheck,
    compact: bool,
) -> El {
    let host_pill = if step.applies_to_host {
        widgets::pill(
            k,
            format!("Runs on {}", widgets::os_name(host_os)),
            theme::status(k).success,
        )
    } else {
        widgets::pill(
            k,
            format!("Skipped on {}", widgets::os_name(host_os)),
            theme::status(k).pending,
        )
    };
    let head = row()
        .w_full()
        .gap(6.0)
        .flex_wrap()
        .center_y()
        .child(
            text(step.name.clone())
                .font(k.ty.small_medium)
                .color(k.c.text_base)
                .wrap()
                .min_w(0.0),
        )
        .child(widgets::pill(k, step.kind.clone(), k.c.text_muted))
        .child(host_pill)
        .child(
            text(format!("{}s", step.effective_timeout))
                .font(k.ty.mono)
                .color(k.c.text_faint)
                .nowrap(),
        );

    let mut box_el = col()
        .key(format!("deploy-step-{idx}"))
        .w_full()
        .padding(10.0)
        .gap(6.0)
        .radius(k.radius.md)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_base)
        .child(head);

    if !step.command_summary.is_empty() {
        box_el = box_el.child(
            text(step.command_summary.clone())
                .font(k.ty.mono)
                .color(k.c.text_muted)
                .wrap_anywhere(),
        );
    }

    for item in &step.checks {
        box_el = box_el.child(check_row(k, item, compact));
    }

    box_el
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Editor;

    #[test]
    fn project_config_index_matches_exact_name_when_loaded() {
        let mut editor = Editor::default();
        assert_eq!(project_config_index(&editor, "beta", 1), Some(1));

        editor.loaded = true;
        editor.draft = serde_json::json!({
            "project": [
                { "name": "beta" },
                { "name": "alpha" }
            ]
        });
        assert_eq!(project_config_index(&editor, "beta", 1), Some(0));
        assert_eq!(project_config_index(&editor, "alpha", 0), Some(1));
        assert_eq!(project_config_index(&editor, "deleted", 1), None);
    }
}
