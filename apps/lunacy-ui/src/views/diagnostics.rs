//! Diagnostics: the daemon's host, its tools, and where its config lives.
//!
//! `os` is the daemon's host OS, not this window's, so it leads the page: it is the one fact
//! that explains why a step with `only_on` was skipped here but ran (or didn't) elsewhere.

use rune::{col, el, icon, row, text, Cx, El, Icon, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::icons;
use crate::state::State;
use crate::theme;
use crate::widgets;

pub fn view(s: &State, k: &Kit, _cx: &mut Cx<State>, compact: bool) -> El {
    let mut c = col()
        .key("diagnostics")
        .w_full()
        .pl(if compact { 14.0 } else { 22.0 })
        .pr(if compact { 18.0 } else { 22.0 })
        .py(if compact { 14.0 } else { 18.0 })
        .gap(20.0);

    if let Some(error) = &s.system_error {
        c = c.child(widgets::notice(k, theme::status(k).failure, error));
    }
    let Some(info) = &s.system else {
        return c.child(widgets::empty_state(
            k,
            "Reading the daemon's host…",
            "`GET /api/system` has not answered yet.",
        ));
    };

    // The host OS, as the page's headline: it decides which `only_on` steps run.
    let mut host_row = row().w_full().gap(10.0).center_y();
    if compact {
        host_row = host_row.flex_wrap();
    }
    host_row = host_row
        .child(
            text(widgets::os_name(&info.os))
                .key("daemon-os")
                .font(rune::TextStyle {
                    size: 19.0,
                    ..k.ty.ui_medium
                })
                .color(k.c.text_base),
        )
        .child(widgets::pill(k, info.os.clone(), k.c.text_accent))
        .child(el().grow(1.0))
        .child(
            Button::new("Refresh")
                .icon(&icons::REFRESH_CW)
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    s.request_system();
                    s.refresh(tx);
                })
                .build(k)
                .key("diagnostics-refresh"),
        );

    c = c.child(
        col()
            .w_full()
            .padding(16.0)
            .gap(6.0)
            .radius(k.radius.lg)
            .fill(k.c.layer_02)
            .border(1.0, k.c.border_base)
            .child(widgets::caption_icon(k, &icons::CPU, "DAEMON HOST"))
            .child(host_row)
            .child(
                text("Steps limited with `only_on` run only when this host is in their list; elsewhere they are recorded as skipped.".to_string())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
            ),
    );

    c = c.child(widgets::section_icon(k, &icons::SERVER, "Daemon", None));
    let mut daemon = col().w_full().gap(0.0);
    daemon = daemon.child(widgets::kv_for(
        k,
        compact,
        "Version",
        widgets::value_for(k, compact, &info.version),
    ));
    daemon = daemon.child(widgets::divider(k));
    daemon = daemon.child(widgets::kv_for(
        k,
        compact,
        "Config file",
        widgets::value_for(k, compact, &info.config_path),
    ));
    if let Some(state_dir) = &info.state_dir {
        daemon = daemon.child(widgets::divider(k));
        daemon = daemon.child(widgets::kv_for(
            k,
            compact,
            "State dir",
            widgets::value_for(k, compact, state_dir),
        ));
    }
    daemon = daemon.child(widgets::divider(k));
    daemon = daemon.child(widgets::kv_for(
        k,
        compact,
        "Server URL",
        widgets::value_for(k, compact, &s.server_display()),
    ));
    if let Some(max_runs) = info.max_concurrent_runs {
        daemon = daemon.child(widgets::divider(k));
        daemon = daemon.child(
            widgets::kv_for(
                k,
                compact,
                "Max concurrent runs",
                widgets::value_for(k, compact, &max_runs.to_string()),
            )
            .key("diagnostics-max-concurrent-runs"),
        );
    }
    if let Some(act_max) = info.act_max_concurrent {
        daemon = daemon.child(widgets::divider(k));
        daemon = daemon.child(
            widgets::kv_for(
                k,
                compact,
                "act concurrency",
                widgets::value_for(k, compact, &act_max.to_string()),
            )
            .key("diagnostics-act-max-concurrent"),
        );
    }
    daemon = daemon.child(widgets::divider(k));
    daemon = daemon.child(
        widgets::kv_for(
            k,
            compact,
            "Transport",
            if s.ws_connected {
                widgets::pill_with_icon(
                    k,
                    &icons::WIFI,
                    "WebSocket (live)",
                    theme::status(k).success,
                )
            } else {
                widgets::pill_with_icon(k, &icons::CLOCK, "HTTP fallback", k.c.text_muted)
            },
        )
        .key("diagnostics-transport"),
    );
    daemon = daemon.child(widgets::divider(k));
    let webhook_val = match info.webhook.as_ref() {
        Some(wh) => {
            let pill = if wh.configured {
                widgets::pill_with_icon(k, &icons::ZAP, "Configured", theme::status(k).success)
            } else {
                widgets::pill(k, "Not configured", k.c.text_faint)
            };
            let mut summary = format!(
                "{} {}",
                wh.deliveries,
                if wh.deliveries == 1 {
                    "delivery"
                } else {
                    "deliveries"
                }
            );
            if let Some(ev) = wh.last_event.as_deref().filter(|v| !v.is_empty()) {
                summary.push_str(&format!(" · last: {ev}"));
                if let Some(repo) = wh.last_repo.as_deref().filter(|v| !v.is_empty()) {
                    summary.push_str(&format!(" ({repo})"));
                }
                if let Some(ts) = wh.last_received_at.as_deref().filter(|v| !v.is_empty()) {
                    summary.push_str(&format!(" at {ts}"));
                }
            }
            let summary_el = text(summary)
                .font(k.ty.small)
                .color(k.c.text_muted)
                .min_w(0.0);
            row()
                .gap(8.0)
                .center_y()
                .flex_wrap()
                .child(pill)
                .child(if compact {
                    summary_el.wrap_anywhere().shrink()
                } else {
                    summary_el.wrap().shrink()
                })
        }
        None => widgets::pill(k, "Not configured", k.c.text_faint),
    };
    daemon = daemon.child(
        widgets::kv_for(k, compact, "GitHub Webhook", webhook_val).key("diagnostics-webhook"),
    );
    c = c.child(daemon);

    c = c.child(widgets::section_icon(
        k,
        &icons::TERMINAL,
        "Tools",
        Some("What the daemon needs to run CI and deploy steps."),
    ));
    let mut tools = col().w_full().gap(0.0);
    tools = tools.child(tool_row(
        k,
        compact,
        &icons::BOX,
        "act",
        info.act.path.as_deref(),
        info.act.version.as_deref(),
    ));
    tools = tools.child(widgets::divider(k));
    tools = tools.child(tool_row(
        k,
        compact,
        &icons::GIT_BRANCH,
        "git",
        None,
        info.git.version.as_deref(),
    ));
    tools = tools.child(widgets::divider(k));
    tools = tools.child(widgets::kv_for(
        k,
        compact,
        "docker",
        match (info.docker.ok, info.docker.error.as_deref()) {
            (true, _) => widgets::pill_with_icon(
                k,
                &icons::CHECK_CIRCLE,
                "Available",
                theme::status(k).success,
            ),
            (false, Some(error)) => {
                let ink = theme::status(k).failure;
                let mut notice = row()
                    .px(8.0)
                    .py(4.0)
                    .gap(6.0)
                    .radius(k.radius.sm)
                    .fill(theme::status(k).tint(ink))
                    .items_start()
                    .min_w(0.0)
                    .shrink()
                    .child(
                        icon(&icons::ALERT_CIRCLE, 13.0)
                            .color(ink)
                            .mt(2.0)
                            .a11y_hidden(),
                    )
                    .child(
                        text(error.to_string())
                            .font(k.ty.small_medium)
                            .color(ink)
                            .wrap_anywhere()
                            .shrink()
                            .min_w(0.0),
                    );
                if compact {
                    notice = notice.w_full();
                }
                notice
            }
            (false, None) => widgets::pill_with_icon(
                k,
                &icons::X_CIRCLE,
                "Not available",
                theme::status(k).failure,
            ),
        },
    ));
    c = c.child(tools);

    c
}

fn tool_row(
    k: &Kit,
    compact: bool,
    ic: &'static Icon,
    name: &str,
    path: Option<&str>,
    version: Option<&str>,
) -> El {
    let found = path.is_some() || version.is_some();
    let detail = match (path, version) {
        (Some(path), Some(version)) => format!("{version} · {path}"),
        (Some(path), None) => path.to_string(),
        (None, Some(version)) => version.to_string(),
        (None, None) => "not found".to_string(),
    };
    let status_pill = if found {
        widgets::pill_with_icon(k, &icons::CHECK_CIRCLE, "Found", theme::status(k).success)
    } else {
        widgets::pill_with_icon(k, &icons::X_CIRCLE, "Missing", theme::status(k).failure)
    };
    if compact {
        return col()
            .w_full()
            .min_h(44.0)
            .py(8.0)
            .gap(4.0)
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .child(icon(ic, 13.0).color(k.c.icon_muted).a11y_hidden())
                    .child(
                        text(name.to_string())
                            .font(k.ty.small_medium)
                            .color(k.c.text_muted)
                            .grow(1.0),
                    )
                    .child(status_pill),
            )
            .child(
                text(detail)
                    .font(k.ty.mono)
                    .color(if found { k.c.text_base } else { k.c.text_faint })
                    .wrap_anywhere(),
            );
    }
    row()
        .w_full()
        .min_h(30.0)
        .gap(12.0)
        .center_y()
        .child(
            row()
                .w(90.0)
                .gap(6.0)
                .center_y()
                .child(icon(ic, 13.0).color(k.c.icon_muted).a11y_hidden())
                .child(
                    text(name.to_string())
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                ),
        )
        .child(
            text(detail)
                .font(k.ty.mono)
                .color(if found { k.c.text_base } else { k.c.text_faint })
                .nowrap()
                .truncate()
                .grow(1.0),
        )
        .child(status_pill)
}
