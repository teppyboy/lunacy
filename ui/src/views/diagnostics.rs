//! Diagnostics: the daemon's host, its tools, and where its config lives.
//!
//! `os` is the daemon's host OS, not this window's, so it leads the page: it is the one fact
//! that explains why a step with `only_on` was skipped here but ran (or didn't) elsewhere.

use rune::{col, el, row, text, Cx, El, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

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
            .child(widgets::caption(k, "DAEMON HOST"))
            .child(host_row)
            .child(
                text("Steps limited with `only_on` run only when this host is in their list; elsewhere they are recorded as skipped.".to_string())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
            ),
    );

    c = c.child(widgets::section(k, "Daemon", None));
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
    c = c.child(daemon);

    c = c.child(widgets::section(
        k,
        "Tools",
        Some("What the daemon needs to run CI and deploy steps."),
    ));
    let mut tools = col().w_full().gap(0.0);
    tools = tools.child(tool_row(
        k,
        compact,
        "act",
        info.act.path.as_deref(),
        info.act.version.as_deref(),
    ));
    tools = tools.child(widgets::divider(k));
    tools = tools.child(tool_row(
        k,
        compact,
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
            (true, _) => widgets::pill(k, "Available", theme::status(k).success),
            (false, Some(error)) => {
                let ink = theme::status(k).failure;
                let mut notice = row()
                    .px(8.0)
                    .py(4.0)
                    .radius(k.radius.sm)
                    .fill(theme::status(k).tint(ink))
                    .items_start()
                    .min_w(0.0)
                    .shrink()
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
            (false, None) => widgets::pill(k, "Not available", theme::status(k).failure),
        },
    ));
    c = c.child(tools);

    c
}

fn tool_row(k: &Kit, compact: bool, name: &str, path: Option<&str>, version: Option<&str>) -> El {
    let found = path.is_some() || version.is_some();
    let detail = match (path, version) {
        (Some(path), Some(version)) => format!("{version} · {path}"),
        (Some(path), None) => path.to_string(),
        (None, Some(version)) => version.to_string(),
        (None, None) => "not found".to_string(),
    };
    let status_pill = if found {
        widgets::pill(k, "Found", theme::status(k).success)
    } else {
        widgets::pill(k, "Missing", theme::status(k).failure)
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
            text(name.to_string())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .w(90.0),
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
