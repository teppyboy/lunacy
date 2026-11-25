//! Diagnostics: the daemon's host, its tools, and where its config lives.
//!
//! `os` is the daemon's host OS, not this window's, so it leads the page: it is the one fact
//! that explains why a step with `only_on` was skipped here but ran (or didn't) elsewhere.

use rune::{col, el, row, text, Cx, El, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit};

use crate::state::State;
use crate::theme;
use crate::widgets;

pub fn view(s: &State, k: &Kit, _cx: &mut Cx<State>) -> El {
    let mut c = col()
        .key("diagnostics")
        .w_full()
        .px(22.0)
        .py(18.0)
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
    c = c.child(
        col()
            .w_full()
            .padding(16.0)
            .gap(6.0)
            .radius(k.radius.lg)
            .fill(k.c.layer_01)
            .border(1.0, k.c.border_base)
            .child(widgets::caption(k, "DAEMON HOST"))
            .child(
                row()
                    .w_full()
                    .gap(10.0)
                    .center_y()
                    .child(
                        text(widgets::os_name(&info.os))
                            .key("daemon-os")
                            .font(rune::TextStyle { size: 19.0, ..k.ty.ui_medium })
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
                                tx.instant();
                            })
                            .build(k),
                    ),
            )
            .child(
                text("Steps limited with `only_on` run only when this host is in their list; elsewhere they are recorded as skipped.".to_string())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
            ),
    );

    c = c.child(widgets::section(k, "Daemon", None));
    let mut daemon = col().w_full().gap(0.0);
    daemon = daemon.child(widgets::kv(k, "Version", widgets::value(k, &info.version)));
    daemon = daemon.child(widgets::divider(k));
    daemon = daemon.child(widgets::kv(
        k,
        "Config file",
        widgets::value(k, &info.config_path),
    ));
    daemon = daemon.child(widgets::divider(k));
    daemon = daemon.child(widgets::kv(
        k,
        "Server URL",
        widgets::value(k, &s.server_display()),
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
        "act",
        info.act.path.as_deref(),
        info.act.version.as_deref(),
    ));
    tools = tools.child(widgets::divider(k));
    tools = tools.child(tool_row(k, "git", None, info.git.version.as_deref()));
    tools = tools.child(widgets::divider(k));
    tools = tools.child(widgets::kv(
        k,
        "docker",
        match (info.docker.ok, info.docker.error.as_deref()) {
            (true, _) => widgets::pill(k, "Available", theme::status(k).success),
            (false, Some(error)) => widgets::pill(k, error.to_string(), theme::status(k).failure),
            (false, None) => widgets::pill(k, "Not available", theme::status(k).failure),
        },
    ));
    c = c.child(tools);

    c
}

fn tool_row(k: &Kit, name: &str, path: Option<&str>, version: Option<&str>) -> El {
    let found = path.is_some() || version.is_some();
    let detail = match (path, version) {
        (Some(path), Some(version)) => format!("{version} · {path}"),
        (Some(path), None) => path.to_string(),
        (None, Some(version)) => version.to_string(),
        (None, None) => "not found".to_string(),
    };
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
        .child(if found {
            widgets::pill(k, "Found", theme::status(k).success)
        } else {
            widgets::pill(k, "Missing", theme::status(k).failure)
        })
}
