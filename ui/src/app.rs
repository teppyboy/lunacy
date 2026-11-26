//! The window: the titlebar, the navigation rail, the connection banner, the status bar and the
//! toast layer. Screens fill the content area; this file only decides which one shows.

use rune::{
    col, el, icon, row, text, Align, AppSpec, Color, Cx, Edge, El, Key, KeyEvent, Mods, ScrollSpec,
    Spring, TextEdit, Transition, Tx,
};
use rune_kit::{glyphs, Kit};

use crate::model::ProjectState;
use crate::state::{Link, SettingsPage, State, ToastKind, View};
use crate::theme;
use crate::views;
use crate::widgets;

// ---------------------------------------------------------------------------------------------
// Launch

/// The app the binary runs.
pub fn build() -> rune::App<State> {
    let app = rune::App::new(rune::AppSpec {
        setup: Some(|s: &mut State, waker: rune::Waker| s.attach(waker)),
        poll: Some(|s: &mut State, clock: f64, tx: &mut Tx| s.poll(clock, tx)),
        on_key: Some(on_key),
        scenes: SCENES,
        min_size: Some((320.0, 480.0)),
        ..AppSpec::new("lunacy", (1180.0, 760.0), init, view)
    })
    .titlebar(rune::Titlebar::new(38.0).controls(true))
    .chrome(rune::Chrome::DEFAULT)
    .full_viewport();

    // Web: the runtime's handle is captured here so `spawn_local` answers can land in `poll`.
    #[cfg(target_arch = "wasm32")]
    let app = {
        let handle = app.handle();
        app.init(move || {
            let mut s = init();
            s.adopt(handle.clone());
            s
        })
    };
    app
}

fn init() -> State {
    crate::updater::cleanup_old_binary();
    let mut s = State::new(crate::state::conn_from_env());
    s.load_prefs();
    s
}

/// One named scene: `--scene` and `tools/shots.sh` pick by name.
pub type Scene = (&'static str, fn(&mut State));

/// Named states, for `--scene` snapshots and the screenshot script.
pub const SCENES: &[Scene] = &[
    ("projects", |s| crate::demo::load(s, View::Projects)),
    ("projects-dark", |s| crate::demo::load(s, View::Projects)),
    ("project", |s| {
        crate::demo::load(
            s,
            View::Project {
                name: "lunarum".into(),
            },
        )
    }),
    ("run", |s| {
        crate::demo::load(
            s,
            View::Run {
                id: "lunarum-1764067434-4f1c2b9".into(),
            },
        )
    }),
    ("run-finished", |s| {
        crate::demo::load(
            s,
            View::Run {
                id: "lunarum-1764064800-9a8b7c6".into(),
            },
        )
    }),
    ("run-failed", |s| {
        crate::demo::load(
            s,
            View::Run {
                id: "lunarum-1764057600-3c4d5e6".into(),
            },
        )
    }),
    ("settings-general", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::General,
            },
        )
    }),
    ("settings-server", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Server,
            },
        )
    }),
    ("settings-github", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Github,
            },
        )
    }),
    ("settings-discord", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Discord,
            },
        )
    }),
    ("settings-act", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Act,
            },
        )
    }),
    ("settings-projects", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Projects,
            },
        )
    }),
    ("settings-step", crate::demo::editor_step),
    ("settings-rejected", crate::demo::rejected),
    ("settings-deploy", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Deploy,
            },
        )
    }),
    ("settings-diagnostics", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Diagnostics,
            },
        )
    }),
    ("settings-connection", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Connection,
            },
        )
    }),
    ("settings-appearance", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Appearance,
            },
        )
    }),
    ("settings-appearance-oled", |s| {
        crate::demo::load(
            s,
            View::Settings {
                page: SettingsPage::Appearance,
            },
        );
        s.theme_mode = crate::theme::ThemeMode::Oled;
    }),
    ("run-oled", |s| {
        let id = crate::demo::failed_run()
            .or_else(|| crate::demo::runs().into_iter().next())
            .map(|r| r.id)
            .unwrap_or_default();
        crate::demo::load(s, View::Run { id });
        s.theme_mode = crate::theme::ThemeMode::Oled;
    }),
    ("offline", crate::demo::offline),
    ("unauthorized", crate::demo::unauthorized),
    ("toast", crate::demo::with_toast),
];

// ---------------------------------------------------------------------------------------------
// Keys

fn is_cmd_key(ev: &KeyEvent, ch: char) -> bool {
    let mut buf = [0u8; 4];
    let target = ch.encode_utf8(&mut buf);
    ev.is_command(ch)
        || ((ev.mods.cmd || ev.mods.ctrl) && matches!(&ev.key, Key::Text(t) if t == target))
}

/// Global keys the focused control didn't take.
fn on_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if is_cmd_key(ev, '=') || is_cmd_key(ev, '+') {
        s.step_zoom(1, tx);
        return true;
    }
    if is_cmd_key(ev, '-') || is_cmd_key(ev, '_') {
        s.step_zoom(-1, tx);
        return true;
    }
    if is_cmd_key(ev, '0') {
        s.set_user_zoom(None, tx);
        return true;
    }
    if ev.mods.alt
        && !ev.mods.ctrl
        && !ev.mods.cmd
        && matches!(&ev.key, Key::Text(t) if t.eq_ignore_ascii_case("z") || t == "Ω")
        && matches!(s.view, View::Run { .. })
    {
        s.toggle_log_wrap(tx);
        return true;
    }
    match ev.key {
        // Ctrl/Cmd+S saves the config, wherever the form is scrolled to.
        Key::Command('s') => {
            if matches!(s.view, View::Settings { .. }) && s.editor.loaded {
                s.save();
                return true;
            }
            false
        }
        // Cmd/Ctrl+R re-reads everything.
        Key::Command('r') => {
            s.reconnect(tx);
            true
        }
        // Cmd/Ctrl+, opens Settings.
        Key::Command(',') => {
            s.open_settings(SettingsPage::General, tx);
            true
        }
        Key::Escape => {
            if s.drawer_open {
                s.drawer_open = false;
                return true;
            }
            if s.editor.step.is_some() {
                s.editor.step = None;
                return true;
            }
            if !matches!(s.view, View::Projects) {
                s.back(tx);
                return true;
            }
            false
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// View

fn view(s: &State, cx: &mut Cx<State>) -> El {
    let gen = rune::commit_generation();
    let (prev_gen, prev_target) = s.zoom_track.get();
    let root_size = cx.layout_size("app-root");
    let zoom = if gen == prev_gen && root_size.is_some() {
        prev_target
    } else {
        let z = match s.user_zoom {
            Some(z) => z,
            None => match root_size {
                Some([lw, _]) => {
                    let window_w = lw * prev_target;
                    if window_w < 720.0 || cx.touch_controls() {
                        1.0
                    } else {
                        1.1
                    }
                }
                None => {
                    if cx.touch_controls() {
                        1.0
                    } else {
                        1.1
                    }
                }
            },
        };
        s.zoom_track.set((gen, z));
        z
    };
    cx.zoom(zoom);

    let compact = cx
        .layout_with("app-root", |r| r[2] < 720.0)
        .unwrap_or_else(|| cx.touch_controls());
    let layout_w = root_size.map_or(if compact { 390.0 } else { 1180.0 }, |sz| sz[0]);
    let dark = cx.system_dark();
    let k = theme::kit_mode(s.theme_mode, dark, compact);
    let scale = cx.env().scale;
    let hair = theme::hair(scale);
    let safe = cx.safe_area().map(|v| (v / zoom).max(0.0));

    let content = if compact {
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(screen(s, &k, cx, hair, compact, layout_w))
    } else {
        row()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(rail(s, &k, hair, compact, layout_w))
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .min_h(0.0)
                    .child(screen(s, &k, cx, hair, compact, layout_w)),
            )
    };

    let body = col()
        .key("app-root")
        .w_full()
        .h_full()
        .fill(k.c.page)
        .pt(safe[0])
        .pr(safe[1])
        .pb(safe[2])
        .pl(safe[3])
        .child(titlebar(s, &k, cx, hair, compact))
        .children(banner(s, &k, hair, compact))
        .children(update_banner(s, &k, hair, compact))
        .child(content);

    let drawer = (compact && s.drawer_open).then(|| drawer_overlays(s, &k, hair, layout_w, safe));
    // Drawer sits below toasts so error notices remain readable if the drawer opens.
    let toasts = toast_layer(s, &k, compact, layout_w, safe);
    rune_kit::root(
        body,
        drawer.into_iter().flatten().chain(std::iter::once(toasts)),
    )
}

/// The window's title row: the app name, the current screen's title, and the connection state.
fn titlebar(s: &State, k: &Kit, cx: &Cx<State>, hair: f32, compact: bool) -> El {
    let inset = cx.chrome_insets();
    let title = match &s.view {
        View::Projects => "Projects".to_string(),
        View::Project { name } => name.clone(),
        View::Run { .. } => s
            .run
            .as_ref()
            .map(|r| r.project.clone())
            .unwrap_or_else(|| "Run".into()),
        View::Settings { page } => format!("Settings · {}", page.title()),
    };
    let _ = hair;
    let mut bar = row()
        .key("titlebar")
        .w_full()
        .h(if compact { 52.0 } else { 46.0 })
        .pl(inset.leading.max(if compact { 8.0 } else { 14.0 }))
        .pr(inset.trailing.max(if compact { 8.0 } else { 14.0 }))
        .gap(if compact { 6.0 } else { 10.0 })
        .center_y()
        .fill(k.c.layer_01);
    if !compact {
        bar = bar.window_drag();
    }
    if compact {
        bar = bar
            .child(
                el().key("titlebar-menu")
                    .role("button")
                    .label("Menu")
                    .size(44.0, 44.0)
                    .radius(k.radius.md)
                    .hover(k.c.hover)
                    .press_scale(0.985)
                    .center()
                    .cursor(rune::Cursor::Pointer)
                    .child(widgets::hamburger(k.c.icon_base))
                    .on_press_tx(|s: &mut State, _tx: &mut Tx| {
                        s.drawer_open = !s.drawer_open;
                    }),
            )
            .child(widgets::dot(k.c.text_accent, 9.0))
            .child(
                text(title)
                    .key("titlebar-title")
                    .font(k.ty.ui_medium)
                    .color(k.c.text_base)
                    .nowrap()
                    .truncate()
                    .grow(1.0),
            );
    } else {
        bar = bar
            .child(
                row()
                    .key("brand")
                    .gap(7.0)
                    .center_y()
                    .child(widgets::dot(k.c.text_accent, 9.0))
                    .child(text("lunacy").font(k.ty.ui_medium).color(k.c.text_base)),
            )
            .child(text("/").font(k.ty.ui).color(k.c.text_faint).a11y_hidden())
            .child(
                text(title)
                    .key("titlebar-title")
                    .font(k.ty.ui)
                    .color(k.c.text_muted)
                    .nowrap()
                    .truncate()
                    .grow(1.0),
            );
    }
    bar.child(
        row()
            .key("titlebar-actions")
            .gap(6.0)
            .center_y()
            .child(connection_chip(s, k, compact))
            .child(widgets::compact_icon(
                rune_kit::IconButton::new(&glyphs::DASH)
                    .label("Settings")
                    .help("Settings (⌘,)")
                    .on_press_tx(|s: &mut State, tx: &mut Tx| {
                        if matches!(s.view, View::Settings { .. }) {
                            s.back(tx);
                        } else {
                            s.open_settings(SettingsPage::General, tx);
                        }
                    })
                    .build(k)
                    .key("titlebar-settings"),
                compact,
            )),
    )
    .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
}

/// The connection chip: dot and label on desktop; compact 44×44 dot pill on phones.
fn connection_chip(s: &State, k: &Kit, compact: bool) -> El {
    let connecting = matches!(s.link, Link::Connecting);
    let (ink, label) = match &s.link {
        Link::Connecting => (k.c.text_muted, "Connecting".to_string()),
        Link::Ok => (theme::status(k).success, "Connected".to_string()),
        Link::Down { .. } => (theme::status(k).failure, "Offline".to_string()),
        Link::Unauthorized => (theme::status(k).error, "Unauthorized".to_string()),
    };
    let indicator = |size| {
        if connecting {
            widgets::live_dot(ink, size)
        } else {
            widgets::dot(ink, size)
        }
    };
    if compact {
        return el()
            .key("connection")
            .role("status")
            .label(label)
            .size(44.0, 44.0)
            .center()
            .radius(k.radius.md)
            .hover(k.c.hover)
            .child(
                el().h(26.0)
                    .px(9.0)
                    .center()
                    .radius(k.radius.md)
                    .fill(k.c.layer_02)
                    .child(indicator(8.0)),
            )
            .on_press_tx(|s: &mut State, tx: &mut Tx| s.open_settings(SettingsPage::Connection, tx))
            .cursor(rune::Cursor::Pointer);
    }
    el().key("connection")
        .role("status")
        .label(label.clone())
        .h(24.0)
        .px(8.0)
        .gap(6.0)
        .center_y()
        .radius(k.radius.md)
        .fill(k.c.layer_02)
        .child(indicator(7.0))
        .child(text(label).font(k.ty.small).color(ink))
        .on_press_tx(|s: &mut State, tx: &mut Tx| s.open_settings(SettingsPage::Connection, tx))
        .cursor(rune::Cursor::Pointer)
}

/// The banner under the titlebar: only shown when the daemon is not answering.
fn banner(s: &State, k: &Kit, hair: f32, compact: bool) -> Option<El> {
    let (ink, title, body) = match &s.link {
        Link::Ok | Link::Connecting => return None,
        Link::Down { why, failures } => (
            theme::status(k).failure,
            "Daemon unreachable".to_string(),
            format!("{why}. Retrying every 2 s ({failures})."),
        ),
        Link::Unauthorized => (
            theme::status(k).error,
            "Unauthorized".to_string(),
            "The token was rejected. Open Connection and check it.".to_string(),
        ),
    };
    let conn_btn = rune_kit::Button::new("Connection")
        .variant(rune_kit::ButtonVariant::Ghost)
        .control_size(rune_kit::ControlSize::Small)
        .on_press_tx(|s: &mut State, tx: &mut Tx| s.open_settings(SettingsPage::Connection, tx))
        .build(k)
        .key("banner-connection");
    let retry_btn = rune_kit::Button::new("Retry")
        .variant(rune_kit::ButtonVariant::Outline)
        .control_size(rune_kit::ControlSize::Small)
        .on_press_tx(|s: &mut State, tx: &mut Tx| s.reconnect(tx))
        .build(k)
        .key("banner-retry");

    let banner_enter = Transition::asymmetric(
        Transition::move_edge(Edge::Top).combined(Transition::opacity()),
        Transition::none(),
    );

    if compact {
        return Some(
            col()
                .key("link-banner")
                .w_full()
                .gap(10.0)
                .px(14.0)
                .py(10.0)
                .fill(theme::status(k).tint(ink))
                .border_sides([0.0, 0.0, hair, 0.0], ink.alpha(0.35))
                .transition(banner_enter)
                .transition_motion(rune::tok::motion::SNAP, Spring::NONE)
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .items_start()
                        .child(widgets::dot(ink, 8.0).mt(5.0))
                        .child(
                            col()
                                .grow(1.0)
                                .min_w(0.0)
                                .gap(2.0)
                                .child(text(title).font(k.ty.small_medium).color(ink))
                                .child(
                                    text(body)
                                        .key("banner-body")
                                        .font(k.ty.small)
                                        .color(ink)
                                        .wrap(),
                                ),
                        ),
                )
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .flex_wrap()
                        .child(conn_btn.grow(1.0))
                        .child(retry_btn.grow(1.0)),
                ),
        );
    }

    Some(
        row()
            .key("link-banner")
            .w_full()
            .gap(10.0)
            .px(16.0)
            .py(9.0)
            .center_y()
            .fill(theme::status(k).tint(ink))
            .border_sides([0.0, 0.0, hair, 0.0], ink.alpha(0.35))
            .transition(banner_enter)
            .transition_motion(rune::tok::motion::SNAP, Spring::NONE)
            .child(widgets::dot(ink, 8.0))
            .child(text(title).font(k.ty.small_medium).color(ink).nowrap())
            .child(
                text(body)
                    .key("banner-body")
                    .font(k.ty.small)
                    .color(ink)
                    .nowrap()
                    .truncate()
                    .min_w(0.0)
                    .grow(1.0),
            )
            .child(conn_btn)
            .child(retry_btn),
    )
}

/// Dismissible desktop auto-updater banner when an update is available or ready to restart.
#[cfg(target_arch = "wasm32")]
fn update_banner(_s: &State, _k: &Kit, _hair: f32, _compact: bool) -> Option<El> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn update_banner(s: &State, k: &Kit, hair: f32, compact: bool) -> Option<El> {
    use crate::updater::UpdateStatus;

    if s.update_dismissed {
        return None;
    }
    let (ink, message, primary_btn, view_url) = match &s.update_status {
        UpdateStatus::Available(info) => {
            let primary = info.asset_url.is_some().then(|| {
                rune_kit::Button::new("Update")
                    .variant(rune_kit::ButtonVariant::Outline)
                    .control_size(rune_kit::ControlSize::Small)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| s.install_update(tx))
                    .build(k)
                    .key("update-banner-install")
            });
            (
                theme::status(k).running,
                format!("Update available ({})", info.short_sha),
                primary,
                (!info.html_url.is_empty()).then(|| info.html_url.clone()),
            )
        }
        UpdateStatus::ReadyToRestart(info) => {
            let primary = Some(
                rune_kit::Button::new("Restart to apply")
                    .variant(rune_kit::ButtonVariant::Submit)
                    .control_size(rune_kit::ControlSize::Small)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| s.restart_for_update(tx))
                    .build(k)
                    .key("update-banner-restart"),
            );
            (
                theme::status(k).success,
                format!("Update {} ready to apply", info.short_sha),
                primary,
                (!info.html_url.is_empty()).then(|| info.html_url.clone()),
            )
        }
        _ => return None,
    };

    let view_btn = view_url.map(|url| {
        rune_kit::Button::new("View")
            .variant(rune_kit::ButtonVariant::Ghost)
            .control_size(rune_kit::ControlSize::Small)
            .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                let _ = tx.open_url(&url);
            })
            .build(k)
            .key("update-banner-view")
    });
    let dismiss_btn = rune_kit::Button::new("Dismiss")
        .variant(rune_kit::ButtonVariant::Ghost)
        .control_size(rune_kit::ControlSize::Small)
        .on_press_tx(|s: &mut State, _tx: &mut Tx| s.dismiss_update_banner())
        .build(k)
        .key("update-banner-dismiss");

    let banner_enter = Transition::asymmetric(
        Transition::move_edge(Edge::Top).combined(Transition::opacity()),
        Transition::none(),
    );

    if compact {
        let mut actions = row().w_full().gap(8.0).flex_wrap();
        if let Some(btn) = primary_btn {
            actions = actions.child(btn.grow(1.0));
        }
        if let Some(btn) = view_btn {
            actions = actions.child(btn.grow(1.0));
        }
        actions = actions.child(dismiss_btn.grow(1.0));
        return Some(
            col()
                .key("update-banner")
                .w_full()
                .gap(8.0)
                .px(14.0)
                .py(10.0)
                .fill(theme::status(k).tint(ink))
                .border_sides([0.0, 0.0, hair, 0.0], ink.alpha(0.35))
                .transition(banner_enter)
                .transition_motion(rune::tok::motion::SNAP, Spring::NONE)
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .center_y()
                        .child(widgets::dot(ink, 8.0))
                        .child(
                            text(message)
                                .font(k.ty.small_medium)
                                .color(ink)
                                .wrap()
                                .min_w(0.0)
                                .grow(1.0),
                        ),
                )
                .child(actions),
        );
    }

    let mut bar = row()
        .key("update-banner")
        .w_full()
        .gap(10.0)
        .px(16.0)
        .py(8.0)
        .center_y()
        .fill(theme::status(k).tint(ink))
        .border_sides([0.0, 0.0, hair, 0.0], ink.alpha(0.35))
        .transition(banner_enter)
        .transition_motion(rune::tok::motion::SNAP, Spring::NONE)
        .child(widgets::dot(ink, 8.0))
        .child(
            text(message)
                .font(k.ty.small_medium)
                .color(ink)
                .nowrap()
                .truncate()
                .min_w(0.0)
                .grow(1.0),
        );
    if let Some(btn) = primary_btn {
        bar = bar.child(btn);
    }
    if let Some(btn) = view_btn {
        bar = bar.child(btn);
    }
    Some(bar.child(dismiss_btn))
}

/// The left rail: Projects, Settings, and the projects themselves.
fn rail(s: &State, k: &Kit, hair: f32, compact: bool, layout_w: f32) -> El {
    let _ = hair;
    let row_h = if compact { 44.0 } else { 30.0 };
    let rail_w = if compact {
        272.0
    } else if layout_w < 960.0 {
        168.0
    } else {
        196.0
    };
    let mut list = col().w_full().gap(1.0).pt(8.0).pb(8.0);
    list = list.child(nav_row(
        k,
        "Projects",
        s.view == View::Projects || matches!(s.view, View::Project { .. }),
        row_h,
        |s: &mut State, tx: &mut Tx| s.open_projects(tx),
    ));
    list = list.child(nav_row(
        k,
        "Settings",
        matches!(s.view, View::Settings { .. }),
        row_h,
        |s: &mut State, tx: &mut Tx| s.open_settings(SettingsPage::General, tx),
    ));

    if !s.projects.is_empty() {
        list = list.child(
            row()
                .w_full()
                .px(14.0)
                .pt(14.0)
                .pb(6.0)
                .child(widgets::caption(k, "PROJECTS")),
        );
        for project in &s.projects {
            let pstate = project.state();
            let st = theme::status(k).for_project(pstate);
            let name = project.name.clone();
            let selected = matches!(&s.view, View::Project { name: open } if open == &name);
            let indicator = if pstate == ProjectState::Running {
                widgets::live_dot(st, 7.0)
            } else {
                widgets::dot(st, 7.0)
            };
            list = list.child(
                row()
                    .key(format!("rail-{}", project.name))
                    .w_full()
                    .h(row_h)
                    .pl(14.0)
                    .pr(18.0)
                    .gap(8.0)
                    .center_y()
                    .fill(if selected {
                        k.c.layer_03
                    } else {
                        Color::TRANSPARENT
                    })
                    .hover(k.c.hover)
                    .press_scale(0.985)
                    .child(indicator)
                    .child(
                        text(project.name.clone())
                            .font(if compact { k.ty.ui } else { k.ty.small })
                            .color(if selected {
                                k.c.text_base
                            } else {
                                k.c.text_muted
                            })
                            .nowrap()
                            .truncate()
                            .min_w(0.0)
                            .grow(1.0),
                    )
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                        s.open_project(&name.clone(), tx)
                    })
                    .cursor(rune::Cursor::Pointer),
            );
        }
    }

    let version_line = format!(
        "daemon {}",
        if s.version.is_empty() {
            "—"
        } else {
            s.version.as_str()
        }
    );

    let mut panel = col()
        .key(if compact { "rail-drawer-body" } else { "rail" })
        .w(rail_w)
        .h_full()
        .shrink()
        .gap(0.0)
        .fill(k.c.layer_01);
    if !compact {
        panel = panel
            .pr(theme::SECTION_BORDER)
            .border_sides([0.0, theme::SECTION_BORDER, 0.0, 0.0], k.c.border_strong);
    }
    panel
        .child(widgets::scroll_bar(
            col()
                .key("rail-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec::default())
                .child(list),
            k,
        ))
        .child(
            col()
                .key("rail-footer")
                .w_full()
                .min_w(0.0)
                .px(14.0)
                .py(10.0)
                .gap(2.0)
                .border_sides([theme::SECTION_BORDER, 0.0, 0.0, 0.0], k.c.border_strong)
                .child(
                    text(version_line)
                        .key("rail-version")
                        .font(k.ty.small)
                        .color(k.c.text_faint)
                        .wrap_anywhere(),
                )
                .child(
                    text(s.server_display())
                        .key("rail-server")
                        .font(k.ty.small)
                        .color(k.c.text_faint)
                        .wrap_anywhere(),
                ),
        )
}

fn drawer_overlays(s: &State, k: &Kit, hair: f32, layout_w: f32, safe: [f32; 4]) -> [El; 2] {
    let drawer_w = (layout_w - safe[3] - 48.0).clamp(220.0, 272.0);
    let drawer_head = row()
        .w_full()
        .h(52.0)
        .px(14.0)
        .gap(8.0)
        .center_y()
        .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
        .child(widgets::dot(k.c.text_accent, 9.0))
        .child(
            text("lunacy")
                .font(k.ty.ui_medium)
                .color(k.c.text_base)
                .grow(1.0),
        )
        .child(widgets::compact_icon(
            rune_kit::IconButton::new(&glyphs::CLOSE)
                .label("Close menu")
                .on_press_tx(|s: &mut State, _tx: &mut Tx| {
                    s.drawer_open = false;
                })
                .build(k)
                .key("drawer-close"),
            true,
        ));
    let drawer_exit = Spring::new(0.15, 0.0);
    let scrim = el()
        .key("drawer-scrim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .fill(Color::BLACK.alpha(0.45))
        .transition(Transition::opacity())
        .transition_motion(rune::tok::motion::GLIDE, drawer_exit)
        .on_press_tx(|s: &mut State, _tx: &mut Tx| {
            s.drawer_open = false;
        });
    let drawer = col()
        .key("rail-drawer")
        .absolute(0.0, 0.0)
        .w(drawer_w + safe[3])
        .h_full()
        .pt(safe[0])
        .pr(theme::SECTION_BORDER)
        .pb(safe[2])
        .pl(safe[3])
        .fill(k.c.layer_01)
        .border_sides([0.0, theme::SECTION_BORDER, 0.0, 0.0], k.c.border_strong)
        .transition(Transition::move_edge(Edge::Leading).combined(Transition::opacity()))
        .transition_motion(rune::tok::motion::GLIDE, drawer_exit)
        .on_press(|_: &mut State| {})
        .child(drawer_head)
        .child(
            rail(s, k, hair, true, layout_w)
                .w_full()
                .grow(1.0)
                .min_h(0.0),
        );
    [scrim, drawer]
}

fn nav_row(k: &Kit, label: &str, selected: bool, h: f32, press: fn(&mut State, &mut Tx)) -> El {
    row()
        .key(format!("nav-{label}"))
        .w_full()
        .h(h)
        .pl(14.0)
        .pr(18.0)
        .center_y()
        .fill(if selected {
            k.c.layer_03
        } else {
            Color::TRANSPARENT
        })
        .hover(k.c.hover)
        .press_scale(0.985)
        .child(text(label).font(k.ty.ui).color(if selected {
            k.c.text_base
        } else {
            k.c.text_muted
        }))
        .on_press_tx(press)
        .cursor(rune::Cursor::Pointer)
}

/// The screen body for the current view.
fn screen(s: &State, k: &Kit, cx: &mut Cx<State>, hair: f32, compact: bool, layout_w: f32) -> El {
    let view = match &s.view {
        View::Projects => views::projects::view(s, k, cx, hair, compact),
        View::Project { name } => views::project::view(s, k, name, cx, hair, compact),
        View::Run { id } => views::run::view(s, k, id, cx, hair, compact, layout_w),
        View::Settings { page } => views::settings::view(s, k, *page, cx, hair, compact, layout_w),
    };
    view.transition(Transition::asymmetric(
        Transition::opacity().combined(Transition::offset(0.0, 6.0)),
        Transition::none(),
    ))
    .transition_motion(rune::tok::motion::SNAP, Spring::NONE)
}

/// The toast layer.
fn toast_layer(s: &State, k: &Kit, compact: bool, layout_w: f32, safe: [f32; 4]) -> El {
    let toast_w = if compact {
        (layout_w - safe[1] - safe[3] - 24.0).clamp(240.0, 360.0)
    } else {
        360.0
    };
    let toast_transition = Transition::offset(0.0, 14.0)
        .combined(Transition::opacity())
        .combined(Transition::scale(0.96));
    let mut stack = col()
        .key("toast-stack")
        .gap(8.0)
        .items_end()
        .animate_layout();
    for toast in &s.toasts {
        let (_ink, glyph) = match toast.kind {
            ToastKind::Info => (k.c.text_accent, k.c.text_accent),
            ToastKind::Success => (theme::status(k).success, theme::status(k).success),
            ToastKind::Error => (theme::status(k).failure, theme::status(k).failure),
        };
        let id = toast.id;
        let close_btn = el()
            .key(format!("toast-close-{id}"))
            .role("button")
            .label("Dismiss")
            .size(
                if compact { 44.0 } else { 18.0 },
                if compact { 44.0 } else { 18.0 },
            )
            .center()
            .radius(k.radius.xs)
            .hover(k.c.hover)
            .child(icon(&glyphs::CLOSE, 12.0).color(k.c.icon_muted))
            .on_press(move |s: &mut State| s.dismiss_toast(id))
            .cursor(rune::Cursor::Pointer);
        stack = stack.child(
            row()
                .key(format!("toast-{id}"))
                .w(toast_w)
                .padding(12.0)
                .gap(10.0)
                .items_start()
                .radius(k.radius.lg)
                .fill(k.c.layer_04)
                .border(1.0, k.c.border_base)
                .box_shadow(rune::Shadow {
                    blur: 24.0,
                    y: 8.0,
                    color: Color::BLACK.alpha(0.28),
                    ..rune::Shadow::default()
                })
                .transition(toast_transition)
                .transition_motion(rune::tok::motion::POP, Spring::new(0.15, 0.0))
                .on_press(|_: &mut State| {})
                .child(widgets::dot(glyph, 8.0).mt(5.0))
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(2.0)
                        .child(
                            text(toast.title.clone())
                                .font(k.ty.small_medium)
                                .color(k.c.text_base)
                                .nowrap()
                                .truncate(),
                        )
                        .child(
                            text(toast.body.clone())
                                .font(k.ty.small)
                                .color(k.c.text_muted)
                                .wrap(),
                        ),
                )
                .child(close_btn),
        );
    }
    el().key("toast-layer")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .pr(if compact { 12.0 } else { 18.0 } + safe[1])
        .pb(if compact { 12.0 } else { 18.0 } + safe[2])
        .pl(if compact { 12.0 + safe[3] } else { 0.0 })
        .justify_end()
        .items_end()
        .child(stack)
}

// `Align` and `Mods` are used by screens; keeping the imports here documents the key contract.
#[allow(unused_imports)]
use {Align as _Align, Mods as _Mods, TextEdit as _TextEdit};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Conn, SettingsPage, State, View};
    use rune::{Env, Key, KeyEvent, Mods, Runtime};

    fn setup_runtime(setup: impl FnOnce(&mut State), w: f32, h: f32) -> Runtime<State> {
        let mut state = State::new(Conn::default());
        setup(&mut state);
        let mut rt = Runtime::new(state, view, (w, h), Env::new(true, 1.0));
        for _ in 0..5 {
            rt.settle_all();
            rt.frame(0.016);
        }
        rt
    }

    fn ancestors_have_key(els: &[rune::ElementInfo], mut idx: usize, key: &str) -> bool {
        let mut guard = 0usize;
        while let Some(parent) = els[idx].parent {
            guard += 1;
            if guard > els.len() {
                break;
            }
            if els[parent].key.as_deref() == Some(key) {
                return true;
            }
            idx = parent;
        }
        false
    }

    fn ancestors_hidden_by_scroll(els: &[rune::ElementInfo], idx: usize) -> bool {
        let _ = (els, idx);
        false
    }

    fn assert_responsive_scene(name: &str, w: f32, h: f32, setup: impl FnOnce(&mut State)) {
        let mut rt = setup_runtime(setup, w, h);
        let zoom = rt.zoom();
        let vw = rt.window_viewport().0 / zoom;
        let els = rt.elements();
        let compact = vw < 720.0;

        let mut overflow_errors = Vec::new();
        let mut cut_errors = Vec::new();
        let mut target_errors = Vec::new();

        for (i, el) in els.iter().enumerate() {
            if el.exiting || el.inert {
                continue;
            }
            let r = el.rect;
            let in_nav_strip = ancestors_have_key(&els, i, "settings-nav-strip");
            let interactive = el.focusable
                || matches!(
                    el.role,
                    "button" | "tab" | "checkbox" | "radio" | "switch" | "textbox"
                );
            if interactive && !in_nav_strip && (r[0] < -0.5 || r[0] + r[2] > vw + 0.5) {
                overflow_errors.push(format!(
                    "{:?} role={} label={:?} rect={:?} > vw={vw:.1}",
                    el.key, el.role, el.label, r
                ));
            }
            let tap_h = if el.role == "textbox" && el.key.as_deref() == Some("input") {
                el.parent.map_or(r[3], |p| els[p].rect[3])
            } else {
                r[3]
            };
            if compact
                && interactive
                && !matches!(el.role, "link")
                && el.key.as_deref() != Some("drawer-scrim")
                && tap_h < 43.9
            {
                target_errors.push(format!(
                    "{:?} role={} label={:?} rect={:?} (tap_h={tap_h:.1}) height < 44",
                    el.key, el.role, el.label, r
                ));
            }
            if compact
                && el.text_cut
                && el.key.as_deref() != Some("titlebar-title")
                && !ancestors_hidden_by_scroll(&els, i)
            {
                cut_errors.push(format!(
                    "{:?} role={} text={:?} rect={:?}",
                    el.key, el.role, el.text, r
                ));
            }
        }

        // Also ensure settings-nav-strip itself fits within viewport width when present.
        for el in &els {
            if el.key.as_deref() == Some("settings-nav-strip") {
                assert!(
                    el.rect[0] >= -0.5 && el.rect[0] + el.rect[2] <= vw + 0.5,
                    "[{name} @ {w}x{h}] settings-nav-strip overflows vw={vw}: {:?}",
                    el.rect
                );
            }
        }

        assert!(
            overflow_errors.is_empty(),
            "[{name} @ {w}x{h}] interactive overflow:\n{}",
            overflow_errors.join("\n")
        );
        assert!(
            target_errors.is_empty(),
            "[{name} @ {w}x{h}] compact touch target < 44pt:\n{}",
            target_errors.join("\n")
        );
        assert!(
            cut_errors.is_empty(),
            "[{name} @ {w}x{h}] clipped text (text_cut):\n{}",
            cut_errors.join("\n")
        );
        let _ = &mut rt;
    }

    #[test]
    fn responsive_zoom_and_breakpoint_do_not_oscillate() {
        for (w, h, want_zoom, want_compact) in [
            (390.0, 844.0, 1.0, true),
            (360.0, 780.0, 1.0, true),
            (768.0, 1024.0, 1.1, true),
            (1180.0, 760.0, 1.1, false),
        ] {
            let mut rt = setup_runtime(|s| crate::demo::load(s, View::Projects), w, h);
            for frame in 0..8 {
                rt.frame(0.016);
                rt.settle_all();
                assert!(
                    (rt.zoom() - want_zoom).abs() < 1e-4,
                    "{w}x{h} frame {frame}: expected zoom {want_zoom}, got {}",
                    rt.zoom()
                );
                assert_eq!(
                    rt.find("titlebar-menu").is_some(),
                    want_compact,
                    "{w}x{h} frame {frame}: menu button compact mismatch"
                );
                assert_eq!(
                    rt.find("rail").is_some(),
                    !want_compact,
                    "{w}x{h} frame {frame}: desktop rail mismatch"
                );
            }
        }
    }

    #[test]
    fn compact_screens_fit_viewport_without_clipping_or_small_targets() {
        let run_id = crate::demo::runs().into_iter().next().expect("demo run").id;
        for (w, h) in [
            (390.0, 844.0),
            (360.0, 780.0),
            (768.0, 1024.0),
            (900.0, 700.0),
            (1180.0, 760.0),
        ] {
            assert_responsive_scene("projects", w, h, |s| crate::demo::load(s, View::Projects));
            assert_responsive_scene("project", w, h, |s| {
                crate::demo::load(
                    s,
                    View::Project {
                        name: "lunarum".into(),
                    },
                )
            });
            let rid = run_id.clone();
            assert_responsive_scene("run", w, h, move |s| {
                crate::demo::load(s, View::Run { id: rid.clone() });
                if let Some(step) = s.open_step {
                    if let Some(entry) = s.logs.get_mut(&(rid, step)) {
                        entry.text.push('\n');
                        entry.text.push_str(&"x".repeat(260));
                    }
                }
            });
            assert_responsive_scene("settings-general", w, h, |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::General,
                    },
                )
            });
            assert_responsive_scene("settings-github", w, h, |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Github,
                    },
                )
            });
            assert_responsive_scene("settings-connection", w, h, |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Connection,
                    },
                );
                s.conn.server = "https://lunacy-izuki.example.com:8443/prefix".into();
                s.live = s.conn.clone();
                s.ui_server = rune::TextEdit::new(s.conn.server.clone());
                s.version = "0.1.0-alpha.12 (abcdef0123)".into();
            });
            assert_responsive_scene("settings-diagnostics", w, h, |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Diagnostics,
                    },
                );
                if let Some(sys) = &mut s.system {
                    sys.docker.ok = false;
                    sys.docker.error = Some(
                        "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?".into(),
                    );
                }
            });
            assert_responsive_scene("settings-deploy", w, h, |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Deploy,
                    },
                )
            });
            assert_responsive_scene("settings-appearance", w, h, |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Appearance,
                    },
                )
            });
            assert_responsive_scene("update-banner", w, h, |s| {
                crate::demo::load(s, View::Projects);
                s.update_status =
                    crate::updater::UpdateStatus::Available(crate::updater::UpdateInfo {
                        tag: "prerelease".into(),
                        title: "Pre-release (f5841e0)".into(),
                        sha: "f5841e0123456789abcdef0123456789abcdef01".into(),
                        short_sha: "f5841e0".into(),
                        html_url: "https://github.com/teppyboy/lunacy/releases/tag/prerelease"
                            .into(),
                        published_at: "2026-11-26T00:00:00Z".into(),
                        notes: String::new(),
                        asset_name: Some("lunacy-linux-x86_64.tar.gz".into()),
                        asset_url: Some("https://example.invalid/asset.tar.gz".into()),
                        checksums_url: Some("https://example.invalid/SHA256SUMS.txt".into()),
                    });
            });
            assert_responsive_scene("settings-step-editor", w, h, |s| {
                crate::demo::editor_step(s);
            });
            assert_responsive_scene("unauthorized", w, h, |s| {
                crate::demo::unauthorized(s);
            });
            if w < 720.0 {
                assert_responsive_scene("drawer-open", w, h, |s| {
                    crate::demo::load(s, View::Projects);
                    s.drawer_open = true;
                });
            }
        }
    }

    #[test]
    fn drawer_opens_and_closes_on_escape_and_navigation() {
        let mut rt = setup_runtime(|s| crate::demo::load(s, View::Projects), 390.0, 844.0);
        let (mx, my) = rt.find("titlebar-menu").expect("menu button");
        rt.pointer_down(mx, my);
        rt.pointer_up(mx, my);
        for _ in 0..3 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(rt.state.drawer_open);
        assert!(rt.find("rail-drawer").is_some());

        let mut tx = rune::Tx::default();
        let consumed = on_key(
            &mut rt.state,
            &KeyEvent {
                key: Key::Escape,
                mods: Mods::default(),
            },
            &mut tx,
        );
        assert!(consumed);
        assert!(!rt.state.drawer_open);

        // Open again and tap Settings row in drawer.
        rt.state.drawer_open = true;
        rt.invalidate();
        for _ in 0..3 {
            rt.frame(0.016);
            rt.settle_all();
        }
        let (sx, sy) = rt.find("nav-Settings").expect("drawer Settings nav");
        rt.pointer_down(sx, sy);
        rt.pointer_up(sx, sy);
        for _ in 0..3 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(!rt.state.drawer_open);
        assert!(matches!(rt.state.view, View::Settings { .. }));
    }

    #[test]
    fn b1_run_log_auto_scrolls_pauses_on_scroll_up_and_resumes_at_bottom() {
        use crate::model::{Phase, Run, RunStatus, Step, StepKind, StepStatus};
        use crate::state::{LogMode, LogState};

        let make_lines = |prefix: &str, count: usize| -> String {
            (1..=count)
                .map(|i| format!("{prefix} line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut rt = setup_runtime(
            |s| {
                let run = Run {
                    id: "r-log".into(),
                    project: "alpha".into(),
                    sha: "abcdef1".into(),
                    status: RunStatus::Running,
                    steps: vec![
                        Step {
                            phase: Phase::Ci,
                            name: "Step 0".into(),
                            kind: StepKind::Shell,
                            status: StepStatus::Running,
                            ..Step::default()
                        },
                        Step {
                            phase: Phase::Ci,
                            name: "Step 1".into(),
                            kind: StepKind::Shell,
                            status: StepStatus::Success,
                            ..Step::default()
                        },
                    ],
                    ..Run::default()
                };
                s.view = View::Run { id: "r-log".into() };
                s.run = Some(run);
                s.open_step = Some(0);
                s.logs.insert(
                    ("r-log".into(), 0),
                    LogState {
                        text: make_lines("s0", 120),
                        loading: false,
                        error: None,
                        mode: Some(LogMode::Tail),
                        offset: 0,
                    },
                );
                s.logs.insert(
                    ("r-log".into(), 1),
                    LogState {
                        text: make_lines("s1", 120),
                        loading: false,
                        error: None,
                        mode: Some(LogMode::Tail),
                        offset: 0,
                    },
                );
            },
            1180.0,
            760.0,
        );

        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));
        // Always-visible scrollbar wraps the viewport.
        assert!(rt.find("run-log-scroll-scroll-area").is_some());

        // Growth while at bottom keeps following bottom.
        rt.update(|s, _| {
            if let Some(e) = s.logs.get_mut(&("r-log".into(), 0)) {
                e.text = make_lines("s0", 160);
            }
        });
        for _ in 0..5 {
            rt.frame(0.016);
        }
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));

        // User scrolls up: stop following.
        let (lx, ly) = rt.find("run-log-scroll").expect("log viewport");
        rt.wheel_xy(lx, ly, 0.0, 400.0);
        for _ in 0..30 {
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(false));

        // More lines arrive while scrolled up: keep reading position, do not jump back.
        rt.update(|s, _| {
            if let Some(e) = s.logs.get_mut(&("r-log".into(), 0)) {
                e.text = make_lines("s0", 200);
            }
        });
        for _ in 0..10 {
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(false));

        // User scrolls back to bottom: resumes following growth.
        rt.wheel_xy(lx, ly, 0.0, -5000.0);
        for _ in 0..30 {
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));
        rt.update(|s, _| {
            if let Some(e) = s.logs.get_mut(&("r-log".into(), 0)) {
                e.text = make_lines("s0", 230);
            }
        });
        for _ in 0..10 {
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));

        // Scroll up again, then switch to Step 1 -> jumps to bottom of Step 1.
        rt.wheel_xy(lx, ly, 0.0, 400.0);
        for _ in 0..30 {
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(false));
        let (s1x, s1y) = rt.find("step-1").expect("step-1");
        rt.pointer_down(s1x, s1y);
        rt.pointer_up(s1x, s1y);
        for _ in 0..10 {
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.state.open_step, Some(1));
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));

        // Scroll up again, then toggle Tail -> Full -> jumps to bottom.
        let (lx, ly) = rt.find("run-log-scroll").expect("log viewport");
        rt.wheel_xy(lx, ly, 0.0, 400.0);
        for _ in 0..30 {
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(false));
        let (fx, fy) = rt.find("run-log-mode/segment1").expect("Full toggle");
        rt.pointer_down(fx, fy);
        rt.pointer_up(fx, fy);
        for _ in 0..10 {
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));
    }

    #[test]
    fn b2_scrollbars_mount_on_every_scroll_pane() {
        let rt_projects = setup_runtime(|s| crate::demo::load(s, View::Projects), 1180.0, 760.0);
        assert!(rt_projects.find("projects-scroll-scroll-area").is_some());
        assert!(rt_projects.find("rail-scroll-scroll-area").is_some());

        let rt_proj = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Project {
                        name: "lunarum".into(),
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_proj.find("project-scroll-scroll-area").is_some());

        let rt_set = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::General,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_set.find("settings-body-scroll-area").is_some());
        assert!(rt_set.find("settings-pages-scroll-scroll-area").is_some());

        let rt_set_compact = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::General,
                    },
                )
            },
            390.0,
            844.0,
        );
        assert!(rt_set_compact.find("settings-body-scroll-area").is_some());
        assert!(rt_set_compact
            .find("settings-nav-strip-scroll-area")
            .is_some());

        let rid = crate::demo::runs().into_iter().next().expect("demo run").id;
        let rt_run = setup_runtime(
            move |s| crate::demo::load(s, View::Run { id: rid }),
            1180.0,
            760.0,
        );
        assert!(rt_run.find("run-steps-scroll-scroll-area").is_some());
        assert!(rt_run.find("run-log-scroll-scroll-area").is_some());
    }

    #[test]
    fn b4_oled_theme_surfaces_borders_and_selector() {
        let oled = theme::kit_mode(theme::ThemeMode::Oled, false, false);
        assert_eq!(oled.c.page.hex(), "#000000ff");
        assert_eq!(oled.c.bg_deep.hex(), "#000000ff");
        assert_eq!(oled.c.layer_01.hex(), "#000000ff");
        assert_eq!(oled.c.layer_02.hex(), "#000000ff");
        assert_eq!(oled.c.bg_base.hex(), "#1a1a1aff");
        assert_ne!(oled.c.bg_base, oled.c.layer_01);
        assert_eq!(oled.c.surface_base.hex(), "#000000ff");
        assert_eq!(oled.c.surface_weak.hex(), "#000000ff");
        assert_eq!(oled.c.surface_disabled.hex(), "#000000ff");
        assert_eq!(oled.c.surface_raised.hex(), "#000000ff");
        assert_eq!(oled.c.surface_raised_base.hex(), "#000000ff");
        assert_eq!(oled.c.bg_button_neutral.hex(), "#000000ff");
        assert_eq!(oled.c.layer_03.hex(), "#141414ff");
        assert_eq!(oled.c.layer_04.hex(), "#1c1c1cff");
        assert_eq!(oled.c.border_base.hex(), "#2e2e2eff");
        assert_eq!(oled.c.border_strong.hex(), "#454545ff");
        let dark = theme::kit(true);
        assert_eq!(theme::status(&oled).success, theme::status(&dark).success);

        let mut rt = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Appearance,
                    },
                )
            },
            1180.0,
            760.0,
        );
        let (ox, oy) = rt
            .find("appearance-theme/choice-oled")
            .expect("OLED choice");
        rt.pointer_down(ox, oy);
        rt.pointer_up(ox, oy);
        for _ in 0..3 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert_eq!(rt.state.theme_mode, theme::ThemeMode::Oled);
        let root_fill = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("app-root"))
            .expect("app-root")
            .fill;
        assert_eq!(root_fill.hex(), "#000000ff");
    }

    #[test]
    fn b5_zoom_picker_and_keyboard_shortcuts_preserve_layout() {
        let mut rt = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Appearance,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!((rt.zoom() - 1.1).abs() < 1e-4);

        let (zx, zy) = rt
            .find("appearance-zoom/choice-1.25")
            .expect("1.25x choice");
        rt.pointer_down(zx, zy);
        rt.pointer_up(zx, zy);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert_eq!(rt.state.user_zoom, Some(1.25));
        assert!((rt.zoom() - 1.25).abs() < 1e-4);

        // Verify zoomed 1.25 layout at 1180x760 has zero horizontal overflow and no cut text on main settings labels.
        let vw = rt.window_viewport().0 / rt.zoom();
        for el in rt.elements() {
            if el.exiting || el.inert {
                continue;
            }
            if el.focusable
                || matches!(
                    el.role,
                    "button" | "tab" | "checkbox" | "radio" | "switch" | "textbox"
                )
            {
                assert!(
                    el.rect[0] >= -0.5 && el.rect[0] + el.rect[2] <= vw + 0.5,
                    "zoom 1.25 overflow on {:?} rect={:?} vw={vw}",
                    el.key,
                    el.rect
                );
            }
        }

        // Ctrl/Cmd+= -> 1.5, Ctrl/Cmd+- -> 1.25 -> 1.1, Ctrl/Cmd+0 -> None (responsive default 1.1)
        let mut tx = rune::Tx::default();
        assert!(on_key(
            &mut rt.state,
            &KeyEvent {
                key: Key::Command('='),
                mods: Mods::default(),
            },
            &mut tx
        ));
        assert_eq!(rt.state.user_zoom, Some(1.5));

        assert!(on_key(
            &mut rt.state,
            &KeyEvent {
                key: Key::Command('-'),
                mods: Mods::default(),
            },
            &mut tx
        ));
        assert_eq!(rt.state.user_zoom, Some(1.25));

        assert!(on_key(
            &mut rt.state,
            &KeyEvent {
                key: Key::Command('0'),
                mods: Mods::default(),
            },
            &mut tx
        ));
        assert_eq!(rt.state.user_zoom, None);
    }

    #[test]
    fn log_word_wrap_toggles_height_and_scrollbars() {
        use crate::model::{Phase, Run, RunStatus, Step, StepKind, StepStatus};
        use crate::state::{LogMode, LogState};

        let long_line = "token_".repeat(60);
        let log_text = (0..60)
            .map(|i| format!("line-{i:02} {long_line}"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut rt = setup_runtime(
            |s| {
                let run = Run {
                    id: "r-wrap".into(),
                    project: "alpha".into(),
                    sha: "abcdef1".into(),
                    status: RunStatus::Success,
                    steps: vec![Step {
                        phase: Phase::Ci,
                        name: "Build".into(),
                        kind: StepKind::Shell,
                        status: StepStatus::Success,
                        ..Step::default()
                    }],
                    ..Run::default()
                };
                s.view = View::Run {
                    id: "r-wrap".into(),
                };
                s.run = Some(run);
                s.open_step = Some(0);
                s.logs.insert(
                    ("r-wrap".into(), 0),
                    LogState {
                        text: log_text,
                        loading: false,
                        error: None,
                        mode: Some(LogMode::Tail),
                        offset: 0,
                    },
                );
            },
            1180.0,
            760.0,
        );

        let log_text_height = |rt: &Runtime<State>| -> f32 {
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some("log-text") && !e.exiting)
                .expect("log-text")
                .rect[3]
        };

        // Default wrapped: vertical scrollbar only, no scroll-bar-x, sticks to bottom.
        assert!(rt.state.log_wrap);
        assert!(rt.find("run-log-scroll-scroll-area/scroll-bar").is_some());
        assert!(rt.find("run-log-scroll-scroll-area/scroll-bar-x").is_none());
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));
        let wrapped_h = log_text_height(&rt);

        // Toggle via run-log-wrap button -> unwrapped: shorter text height, both v + h scrollbars.
        let (wx, wy) = rt.find("run-log-wrap").expect("run-log-wrap");
        rt.pointer_down(wx, wy);
        rt.pointer_up(wx, wy);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(!rt.state.log_wrap);
        assert!(rt.find("run-log-scroll-scroll-area/scroll-bar").is_some());
        assert!(rt.find("run-log-scroll-scroll-area/scroll-bar-x").is_some());
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));
        let unwrapped_h = log_text_height(&rt);
        assert!(
            wrapped_h > unwrapped_h * 1.5,
            "wrapped height ({wrapped_h}) should significantly exceed unwrapped height ({unwrapped_h})"
        );

        // Toggle Tail -> Full while unwrapped -> stays at bottom.
        let (fx, fy) = rt.find("run-log-mode/segment1").expect("Full toggle");
        rt.pointer_down(fx, fy);
        rt.pointer_up(fx, fy);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));

        // Toggle back via Alt+Z in Run view and verify re-wrap keeps bottom alignment.
        let mut tx = rune::Tx::default();
        let alt_z = KeyEvent {
            key: Key::Text("z".into()),
            mods: Mods {
                alt: true,
                ..Mods::default()
            },
        };
        assert!(on_key(&mut rt.state, &alt_z, &mut tx));
        assert!(rt.state.log_wrap);
        rt.update(|_, _| {});
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(rt.find("run-log-scroll-scroll-area/scroll-bar-x").is_none());
        assert_eq!(rt.scroll_at_bottom("run-log-scroll"), Some(true));

        // Switch to Settings -> Appearance: Alt+Z should NOT toggle outside Run view,
        // and appearance-log-wrap switch should toggle it cleanly.
        rt.update(|s, tx| s.open_settings(SettingsPage::Appearance, tx));
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(!on_key(&mut rt.state, &alt_z, &mut tx));
        assert!(rt.state.log_wrap);

        let (sx, sy) = rt
            .find("appearance-log-wrap")
            .expect("appearance-log-wrap switch");
        rt.pointer_down(sx, sy);
        rt.pointer_up(sx, sy);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(!rt.state.log_wrap);

        // Restore default wrapped preference.
        rt.pointer_down(sx, sy);
        rt.pointer_up(sx, sy);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(rt.state.log_wrap);
    }

    #[test]
    fn animations_run_on_drawer_open_close_and_toast() {
        let mut rt = setup_runtime(
            |s| {
                crate::demo::load(s, View::Projects);
                // Keep only idle/finished projects so no background loader keeps frames active.
                for p in &mut s.projects {
                    p.running = false;
                    p.current_run = None;
                }
            },
            390.0,
            844.0,
        );
        rt.settle_all();
        assert!(
            !rt.frame(0.016),
            "settled idle screen should not keep requesting frames"
        );

        // Drawer open animates smoothly before settle_all().
        let (mx, my) = rt.find("titlebar-menu").expect("titlebar-menu");
        rt.pointer_down(mx, my);
        rt.pointer_up(mx, my);
        assert!(
            rt.frame(0.016),
            "drawer open should animate instead of snapping"
        );
        assert!(rt.state.drawer_open);
        assert!(rt.find("rail-drawer").is_some());

        // Settle, tap non-interactive space inside rail-drawer (must NOT close it), then close via scrim.
        rt.settle_all();
        rt.frame(0.016);
        let drawer_rect = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("rail-drawer") && !e.exiting)
            .expect("rail-drawer")
            .rect;
        // Bottom-left empty area inside rail-drawer (footer/padding, not a nav item or close button).
        rt.pointer_down(
            drawer_rect[0] + 24.0,
            drawer_rect[1] + drawer_rect[3] - 24.0,
        );
        rt.pointer_up(
            drawer_rect[0] + 24.0,
            drawer_rect[1] + drawer_rect[3] - 24.0,
        );
        rt.frame(0.016);
        assert!(
            rt.state.drawer_open,
            "tapping non-interactive space inside rail-drawer must not close it"
        );

        let (_sx, sy) = rt.find("drawer-scrim").expect("drawer-scrim");
        // Tap on the right side of the scrim outside the drawer.
        rt.pointer_down(360.0, sy);
        rt.pointer_up(360.0, sy);
        assert!(rt.frame(0.016), "drawer close should animate out");
        assert!(!rt.state.drawer_open);
        for _ in 0..14 {
            rt.frame(0.016);
        }
        assert!(
            rt.find("rail-drawer").is_none(),
            "drawer exit should finish well within ~0.2s"
        );

        // Toast show animates before settle_all().
        rt.settle_all();
        rt.frame(0.016);
        assert!(!rt.frame(0.016));
        rt.update(|s, tx| {
            s.toast(tx, ToastKind::Info, "Queued", "alpha: a run was queued.");
        });
        assert!(
            rt.frame(0.016),
            "toast enter should animate instead of snapping"
        );
        assert!(rt.find("toast-1").is_some());
    }

    #[test]
    fn cancel_buttons_and_cancelled_status_render_across_views() {
        use crate::model::{Phase, Run, RunStatus, Step, StepKind, StepStatus};

        // Projects list shows cancel button for running project.
        let rt_projects = setup_runtime(|s| crate::demo::load(s, View::Projects), 1180.0, 760.0);
        assert!(rt_projects.find("cancel-lunarum").is_some());
        assert!(rt_projects.find("cancel-dotfiles").is_none());

        // Project detail shows cancel button for running project.
        let rt_project = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Project {
                        name: "lunarum".into(),
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_project.find("project-cancel").is_some());

        // Run detail shows cancel button for running run and clicking it marks cancelling.
        let running = crate::demo::running_run().expect("running demo run");
        let rid = running.id.clone();
        let mut rt_run = setup_runtime(
            move |s| crate::demo::load(s, View::Run { id: rid }),
            1180.0,
            760.0,
        );
        let (cx, cy) = rt_run.find("run-cancel").expect("run-cancel button");
        rt_run.pointer_down(cx, cy);
        rt_run.pointer_up(cx, cy);
        for _ in 0..3 {
            rt_run.frame(0.016);
            rt_run.settle_all();
        }
        assert_eq!(
            rt_run.state.cancelling.as_deref(),
            Some(running.id.as_str())
        );
        let has_cancelling_text = rt_run
            .elements()
            .into_iter()
            .any(|e| e.text.as_deref() == Some("Cancelling…"));
        assert!(
            has_cancelling_text,
            "expected Cancelling… label after click"
        );

        // Cancelled run renders Cancelled pill and no Cancel button.
        let rt_cancelled = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Run {
                        id: "r-cancel".into(),
                    },
                );
                s.run = Some(Run {
                    id: "r-cancel".into(),
                    project: "lunarum".into(),
                    sha: "abcdef1".into(),
                    subject: "test cancel".into(),
                    status: RunStatus::Cancelled,
                    started_at: 100,
                    finished_at: Some(120),
                    steps: vec![Step {
                        phase: Phase::Ci,
                        name: "Long step".into(),
                        status: StepStatus::Cancelled,
                        kind: StepKind::Shell,
                        ..Step::default()
                    }],
                    ..Run::default()
                });
            },
            1180.0,
            760.0,
        );
        assert!(rt_cancelled.find("run-cancel").is_none());
        let has_cancelled_text = rt_cancelled
            .elements()
            .into_iter()
            .any(|e| e.text.as_deref() == Some("Cancelled"));
        assert!(has_cancelled_text, "expected Cancelled status text");
    }

    #[test]
    fn timeout_fields_and_step_inherited_help_render() {
        let rt_gen = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::General,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_gen.find("field-general.step_timeout").is_some());

        let rt_proj = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Projects,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_proj.find("field-project[0].step_timeout").is_some());
        assert!(rt_proj.find("field-project[0].timeout").is_some());

        // Step editor shows inherited project/global timeout instead of "waits forever".
        let rt_step = setup_runtime(
            |s| {
                crate::demo::editor_step(s);
                crate::paths::set_u64(&mut s.editor.draft, "project[0].step_timeout", Some(1200));
            },
            1180.0,
            760.0,
        );
        let texts: Vec<String> = rt_step
            .elements()
            .into_iter()
            .filter_map(|e| e.text)
            .collect();
        let joined = texts.join("\n");
        assert!(
            joined.contains("1200s"),
            "expected inherited 1200s timeout help text, got: {joined}"
        );
        assert!(
            !joined.contains("waits forever"),
            "misleading waits forever text must be removed"
        );
    }

    #[test]
    fn deploy_settings_and_desktop_updater_ui_render_and_interact() {
        let mut rt_dep = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Deploy,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_dep.find("deploy-checks").is_some());
        assert!(rt_dep.find("deploy-refresh").is_some());
        assert!(rt_dep.find("deploy-project-lunarum").is_some());
        assert!(rt_dep.find("deploy-project-dotfiles").is_some());
        // Save/Discard are hidden on read-only local Deploy page.
        assert!(rt_dep.find("settings-save").is_none());

        // Click Re-check button.
        let (rx, ry) = rt_dep.find("deploy-refresh").expect("deploy-refresh");
        rt_dep.pointer_down(rx, ry);
        rt_dep.pointer_up(rx, ry);
        for _ in 0..3 {
            rt_dep.frame(0.016);
            rt_dep.settle_all();
        }
        assert!(rt_dep.state.deploy_checks.is_some());

        // Appearance desktop updates section & dismissible update banner.
        let mut rt_app = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Appearance,
                    },
                );
                s.update_status =
                    crate::updater::UpdateStatus::Available(crate::updater::UpdateInfo {
                        tag: "prerelease".into(),
                        title: "Pre-release (f5841e0)".into(),
                        sha: "f5841e0123456789abcdef0123456789abcdef01".into(),
                        short_sha: "f5841e0".into(),
                        html_url: "https://github.com/teppyboy/lunacy/releases/tag/prerelease"
                            .into(),
                        published_at: "2026-11-26T00:00:00Z".into(),
                        notes: String::new(),
                        asset_name: Some("lunacy-linux-x86_64.tar.gz".into()),
                        asset_url: Some("https://example.invalid/asset.tar.gz".into()),
                        checksums_url: Some("https://example.invalid/SHA256SUMS.txt".into()),
                    });
            },
            1180.0,
            760.0,
        );
        assert!(rt_app.find("desktop-updates").is_some());
        assert!(rt_app.find("appearance-auto-update").is_some());
        assert!(rt_app.find("update-check-now").is_some());
        assert!(rt_app.find("update-banner").is_some());

        let (dx, dy) = rt_app
            .find("update-banner-dismiss")
            .expect("update-banner-dismiss");
        rt_app.pointer_down(dx, dy);
        rt_app.pointer_up(dx, dy);
        for _ in 0..4 {
            rt_app.frame(0.016);
            rt_app.settle_all();
        }
        assert!(rt_app.state.update_dismissed);
        assert!(rt_app.find("update-banner").is_none());
    }

    #[test]
    fn expandable_substep_accordion_and_raw_switch() {
        let running = crate::demo::running_run().expect("running demo run");
        let rid = running.id.clone();
        let mut rt = setup_runtime(
            move |s| crate::demo::load(s, View::Run { id: rid }),
            1180.0,
            760.0,
        );
        // Defaults to Steps accordion view with running substep (index 5: "Test") auto-expanded,
        // while passed substeps (e.g. index 1: "Run actions/checkout@v4") start collapsed.
        assert!(!rt.state.log_view_raw);
        assert!(rt.find("run-log-view").is_some());
        assert!(rt.find("substeps").is_some());
        assert!(rt.find("substep-log-5").is_some());
        assert!(rt.find("substep-log-1").is_none());

        // Click substep 1 to expand its sliced log.
        let (x, y) = rt.find("substep-toggle-1").expect("substep-toggle-1");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(rt.find("substep-log-1").is_some());
        assert!(
            rt.find("substep-log-5").is_some(),
            "previously auto-expanded running substep stays open"
        );
        let has_git_ver = rt
            .elements()
            .into_iter()
            .filter_map(|e| e.text)
            .any(|t| t.contains("git version 2.43.0"));
        assert!(
            has_git_ver,
            "expected sliced Checkout log inside substep-log-1"
        );

        // Click substep 1 again to collapse it.
        let (x, y) = rt.find("substep-toggle-1").expect("substep-toggle-1");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(rt.find("substep-log-1").is_none());

        // Expand all -> opens passed substeps too; click again -> Collapse all.
        let (ex, ey) = rt.find("substeps-expand-all").expect("substeps-expand-all");
        rt.pointer_down(ex, ey);
        rt.pointer_up(ex, ey);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(rt.find("substep-log-1").is_some());
        let (ex, ey) = rt.find("substeps-expand-all").expect("substeps-expand-all");
        rt.pointer_down(ex, ey);
        rt.pointer_up(ex, ey);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(rt.find("substep-log-1").is_none());
        assert!(rt.find("substep-log-5").is_none());

        // Switch Steps -> Raw -> Steps.
        let (rx, ry) = rt.find("run-log-view/segment1").expect("Raw segment");
        rt.pointer_down(rx, ry);
        rt.pointer_up(rx, ry);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(rt.state.log_view_raw);
        assert!(rt.find("log").is_some());
        assert!(rt.find("substeps").is_none());

        let (sx, sy) = rt.find("run-log-view/segment0").expect("Steps segment");
        rt.pointer_down(sx, sy);
        rt.pointer_up(sx, sy);
        for _ in 0..4 {
            rt.frame(0.016);
            rt.settle_all();
        }
        assert!(!rt.state.log_view_raw);
        assert!(rt.find("substeps").is_some());
    }

    #[test]
    fn rail_footer_never_overflows_and_narrow_desktop_adapts() {
        let rid = crate::demo::running_run().expect("running demo run").id;
        for (w, h, expect_rail_w, expect_stacked) in
            [(1180.0, 760.0, 196.0, false), (800.0, 600.0, 168.0, true)]
        {
            let rid_clone = rid.clone();
            let rt = setup_runtime(
                move |s| {
                    crate::demo::load(s, View::Run { id: rid_clone });
                    s.conn.server = "https://lunacy-izuki.tretrauit.me".into();
                    s.live = s.conn.clone();
                    s.version = "0.2.0-alpha.12".into();
                },
                w,
                h,
            );
            let els = rt.elements();
            let rail = els
                .iter()
                .find(|e| e.key.as_deref() == Some("rail") && !e.exiting)
                .expect("rail");
            assert!(
                (rail.rect[2] - expect_rail_w).abs() < 1.0,
                "{w}x{h}: expected rail width {expect_rail_w}, got {}",
                rail.rect[2]
            );
            let rail_right = rail.rect[0] + rail.rect[2];
            for key in ["rail-footer", "rail-version", "rail-server"] {
                let node = els
                    .iter()
                    .find(|e| e.key.as_deref() == Some(key) && !e.exiting)
                    .unwrap_or_else(|| panic!("{key} missing at {w}x{h}"));
                assert!(
                    node.rect[0] + node.rect[2] <= rail_right + 0.5,
                    "{w}x{h}: {key} overflows rail right edge ({rail_right:.1}): {:?}",
                    node.rect
                );
            }
            let steps = els
                .iter()
                .find(|e| e.key.as_deref() == Some("run-steps") && !e.exiting)
                .expect("run-steps");
            let log = els
                .iter()
                .find(|e| e.key.as_deref() == Some("run-log") && !e.exiting)
                .expect("run-log");
            if expect_stacked {
                assert!(
                    log.rect[1] >= steps.rect[1] + steps.rect[3] - 1.0,
                    "{w}x{h}: expected run-steps stacked above run-log: steps={:?} log={:?}",
                    steps.rect,
                    log.rect
                );
            } else {
                assert!(
                    log.rect[0] >= steps.rect[0] + steps.rect[2] - 1.0,
                    "{w}x{h}: expected run-steps beside run-log: steps={:?} log={:?}",
                    steps.rect,
                    log.rect
                );
            }
        }
    }

    #[test]
    fn wave2_pr_force_deploy_and_cache_controls_render_and_interact() {
        // Project view shows open PRs and Force deploy button.
        let rt_proj = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Project {
                        name: "lunarum".into(),
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_proj.find("project-open-prs").is_some());
        assert!(rt_proj.find("project-force-deploy").is_some());

        // Finished non-PR run with deploy steps renders Force deploy, disabled while project is running.
        let finished = crate::demo::finished_run().expect("finished demo run");
        let fid = finished.id.clone();
        let mut rt_run = setup_runtime(
            move |s| crate::demo::load(s, View::Run { id: fid }),
            1180.0,
            760.0,
        );
        let (dx, dy) = rt_run
            .find("run-force-deploy")
            .expect("run-force-deploy on finished non-PR run");
        rt_run.pointer_down(dx, dy);
        rt_run.pointer_up(dx, dy);
        for _ in 0..3 {
            rt_run.frame(0.016);
            rt_run.settle_all();
        }
        assert_eq!(rt_run.state.force_deploying, None);

        // Once the project is idle, clicking Force deploy sets in-flight state.
        for p in &mut rt_run.state.projects {
            p.running = false;
            p.current_run = None;
        }
        rt_run.frame(0.016);
        rt_run.settle_all();
        let (dx, dy) = rt_run
            .find("run-force-deploy")
            .expect("run-force-deploy enabled when project is idle");
        rt_run.pointer_down(dx, dy);
        rt_run.pointer_up(dx, dy);
        for _ in 0..3 {
            rt_run.frame(0.016);
            rt_run.settle_all();
        }
        assert_eq!(
            rt_run.state.force_deploying.as_deref(),
            Some(finished.id.as_str())
        );

        // Finished non-PR run without any deploy steps hides run-force-deploy.
        let rt_ci_only = setup_runtime(
            |s| {
                let mut ci_run = crate::demo::finished_run().expect("finished run");
                ci_run.id = "lunarum-ci-only".into();
                ci_run
                    .steps
                    .retain(|st| st.phase != crate::model::Phase::Deploy);
                s.view = View::Run {
                    id: ci_run.id.clone(),
                };
                s.run = Some(ci_run);
            },
            1180.0,
            760.0,
        );
        assert!(rt_ci_only.find("run-force-deploy").is_none());

        // Startup-window running project (`running == true && current_run.is_none()`) still shows Cancel.
        let mut rt_startup_cancel = setup_runtime(
            |s| {
                crate::demo::load(s, View::Projects);
                if let Some(p) = s.projects.iter_mut().find(|p| p.name == "lunarum") {
                    p.running = true;
                    p.current_run = None;
                }
            },
            1180.0,
            760.0,
        );
        let (cx, cy) = rt_startup_cancel
            .find("cancel-lunarum")
            .expect("cancel button during startup window on Projects view");
        rt_startup_cancel.pointer_down(cx, cy);
        rt_startup_cancel.pointer_up(cx, cy);
        for _ in 0..3 {
            rt_startup_cancel.frame(0.016);
            rt_startup_cancel.settle_all();
        }
        assert_eq!(
            rt_startup_cancel.state.cancelling.as_deref(),
            Some("lunarum")
        );

        // PR run shows PR #42 pill, deploy_skipped_reason, and hides run-force-deploy.
        let rt_pr = setup_runtime(
            |s| {
                let mut pr_run = crate::demo::finished_run().expect("finished run");
                pr_run.id = "lunarum-pr-42".into();
                pr_run.pr_number = Some(42);
                pr_run.deploy_skipped_reason =
                    Some("Pull request builds run CI only; deploy steps are skipped.".into());
                s.view = View::Run {
                    id: pr_run.id.clone(),
                };
                s.run = Some(pr_run);
            },
            1180.0,
            760.0,
        );
        assert!(rt_pr.find("run-force-deploy").is_none());
        assert!(rt_pr.find("run-deploy-skipped").is_some());
        assert!(rt_pr
            .elements()
            .into_iter()
            .any(|e| e.text.as_deref() == Some("PR #42")));

        // Deploy page shows Force deploy for projects with deploy steps.
        let rt_dep = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Deploy,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_dep.find("deploy-force-lunarum").is_some());
        assert!(rt_dep.find("deploy-force-dotfiles").is_some());

        // Settings -> act shows cache toggle, cache_dir, and Clear all build caches button.
        let mut rt_act = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Act,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_act.find("field-act.cache").is_some());
        assert!(rt_act.find("field-act.cache_dir").is_some());
        // Scroll down so the Clear all build caches button is inside the visible viewport.
        let (bx, by) = rt_act.find("settings-body").expect("settings-body");
        rt_act.wheel_xy(bx, by, 0.0, -800.0);
        for _ in 0..10 {
            rt_act.frame(0.016);
            rt_act.settle_all();
        }
        let (cx, cy) = rt_act.find("act-clear-cache").expect("act-clear-cache");
        rt_act.pointer_down(cx, cy);
        rt_act.pointer_up(cx, cy);
        for _ in 0..3 {
            rt_act.frame(0.016);
            rt_act.settle_all();
        }
        assert!(rt_act.state.clearing_cache);

        // Settings -> Projects detail shows watch_prs, deploy_refs, watch_tags, cache, and Clear project cache.
        let rt_pform = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Projects,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_pform.find("field-project[0].watch_prs").is_some());
        assert!(rt_pform.find("field-project[0].deploy_refs").is_some());
        assert!(rt_pform.find("field-project[0].watch_tags").is_some());
        assert!(rt_pform.find("field-project[0].cache").is_some());
        assert!(rt_pform.find("project-clear-cache").is_some());
    }

    #[test]
    fn stepped_surface_hierarchy_across_dark_light_and_oled() {
        let assert_achromatic = |name: &str, c: Color| {
            assert!(
                (c.r - c.g).abs() < 1e-6 && (c.g - c.b).abs() < 1e-6,
                "{name} must be pure neutral gray (r==g==b), got {:?}",
                c
            );
        };
        let assert_all_neutral_tokens = |label: &str, k: &Kit| {
            let c = &k.c;
            assert_ne!(
                c.bg_base, c.layer_01,
                "{label}: bg_base must differ from layer_01 for SegmentedControl active pill contrast"
            );
            for (field, col) in [
                ("page", c.page),
                ("bg_base", c.bg_base),
                ("bg_deep", c.bg_deep),
                ("layer_01", c.layer_01),
                ("layer_02", c.layer_02),
                ("layer_03", c.layer_03),
                ("layer_04", c.layer_04),
                ("bg_inverse", c.bg_inverse),
                ("bg_contrast", c.bg_contrast),
                ("bg_button_neutral", c.bg_button_neutral),
                ("bg_icon_button_contrast", c.bg_icon_button_contrast),
                ("submit_fg", c.submit_fg),
                ("submit_icon", c.submit_icon),
                ("text_base", c.text_base),
                ("text_muted", c.text_muted),
                ("text_faint", c.text_faint),
                ("text_inverse", c.text_inverse),
                ("text_contrast", c.text_contrast),
                ("text_strong", c.text_strong),
                ("text_weak", c.text_weak),
                ("text_invert_strong", c.text_invert_strong),
                ("icon_base", c.icon_base),
                ("icon_muted", c.icon_muted),
                ("icon_faint", c.icon_faint),
                ("icon_inverse", c.icon_inverse),
                ("icon_contrast", c.icon_contrast),
                ("icon_weaker", c.icon_weaker),
                ("border_muted", c.border_muted),
                ("border_base", c.border_base),
                ("border_strong", c.border_strong),
                ("border_weak", c.border_weak),
                ("border_hover", c.border_hover),
                ("border_disabled", c.border_disabled),
                ("hover", c.hover),
                ("pressed", c.pressed),
                ("surface_base", c.surface_base),
                ("surface_weak", c.surface_weak),
                ("surface_hover", c.surface_hover),
                ("surface_raised", c.surface_raised),
                ("surface_raised_base", c.surface_raised_base),
                ("surface_raised_hover", c.surface_raised_hover),
                ("surface_float", c.surface_float),
                ("surface_disabled", c.surface_disabled),
                ("std_border", c.std_border),
                ("std_icon", c.std_icon),
                ("std_icon_invert", c.std_icon_invert),
                ("std_icon_strong", c.std_icon_strong),
                ("std_text", c.std_text),
                ("knob", c.knob),
                ("knob_on", c.knob_on),
            ] {
                assert_achromatic(&format!("{label}.{field}"), col);
            }
        };

        let dark = theme::kit_mode(theme::ThemeMode::Dark, true, false);
        assert_eq!(dark.c.bg_deep.hex(), "#0a0a0aff");
        assert_eq!(dark.c.page.hex(), "#0f0f0fff");
        assert_eq!(dark.c.layer_01.hex(), "#141414ff");
        assert_eq!(dark.c.layer_02.hex(), "#1a1a1aff");
        assert_eq!(dark.c.bg_base.hex(), "#242424ff");
        assert_eq!(dark.c.layer_03.hex(), "#242424ff");
        assert_eq!(dark.c.layer_04.hex(), "#2e2e2eff");
        assert_eq!(dark.c.border_base.hex(), "#333333ff");
        assert_eq!(dark.c.border_strong.hex(), "#454545ff");
        assert_all_neutral_tokens("dark", &dark);

        let light = theme::kit_mode(theme::ThemeMode::Light, false, false);
        assert_eq!(light.c.bg_deep.hex(), "#ffffffff");
        assert_eq!(light.c.page.hex(), "#fafafaff");
        assert_eq!(light.c.layer_01.hex(), "#efefefff");
        assert_eq!(light.c.bg_base.hex(), "#ffffffff");
        assert_eq!(light.c.layer_02.hex(), "#ffffffff");
        assert_eq!(light.c.layer_03.hex(), "#e8e8e8ff");
        assert_eq!(light.c.layer_04.hex(), "#ffffffff");
        assert_eq!(light.c.border_strong.hex(), "#999999ff");
        assert_all_neutral_tokens("light", &light);

        let oled = theme::kit_mode(theme::ThemeMode::Oled, true, false);
        assert_eq!(oled.c.page.hex(), "#000000ff");
        assert_eq!(oled.c.bg_deep.hex(), "#000000ff");
        assert_eq!(oled.c.layer_01.hex(), "#000000ff");
        assert_eq!(oled.c.layer_02.hex(), "#000000ff");
        assert_eq!(oled.c.surface_base.hex(), "#000000ff");
        assert_eq!(oled.c.bg_base.hex(), "#1a1a1aff");
        assert_eq!(oled.c.layer_03.hex(), "#141414ff");
        assert_eq!(oled.c.layer_04.hex(), "#1c1c1cff");
        assert_eq!(oled.c.border_base.hex(), "#2e2e2eff");
        assert_eq!(oled.c.border_strong.hex(), "#454545ff");
        assert_all_neutral_tokens("oled", &oled);
    }

    #[test]
    fn selected_sidebar_and_pane_rows_respect_right_border_inset() {
        let assert_inset = |rt: &Runtime<State>, pane_key: &str, row_key: &str| {
            let els = rt.elements();
            let pane = els
                .iter()
                .find(|e| e.key.as_deref() == Some(pane_key) && !e.exiting)
                .unwrap_or_else(|| panic!("missing pane {pane_key}"));
            let row = els
                .iter()
                .find(|e| e.key.as_deref() == Some(row_key) && !e.exiting)
                .unwrap_or_else(|| panic!("missing row {row_key}"));
            let pane_right = pane.rect[0] + pane.rect[2];
            let row_right = row.rect[0] + row.rect[2];
            assert!(
                row_right <= pane_right - theme::SECTION_BORDER + 0.01,
                "{row_key} right edge ({row_right:.2}) overlaps {pane_key} right border (pane_right={pane_right:.2}, border={})",
                theme::SECTION_BORDER
            );
        };

        // 1) Left rail selected project item (`rail-lunarum`).
        let rt_proj = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Project {
                        name: "lunarum".into(),
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert_inset(&rt_proj, "rail", "rail-lunarum");
        assert_inset(&rt_proj, "rail", "nav-Projects");

        // 2) Run steps desktop pane (`step-0` selected).
        let rid = crate::demo::running_run().expect("running demo run").id;
        let rt_run = setup_runtime(
            move |s| {
                crate::demo::load(s, View::Run { id: rid });
                s.open_step = Some(0);
            },
            1180.0,
            760.0,
        );
        assert_inset(&rt_run, "run-steps", "step-0");

        // 3) Settings desktop sidebar (`settings-nav-General` selected).
        let rt_set = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::General,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert_inset(&rt_set, "settings-pages", "settings-nav-General");

        // 4) Compact drawer (`rail-drawer` right border).
        let rt_drawer = setup_runtime(
            |s| {
                crate::demo::load(s, View::Projects);
                s.drawer_open = true;
            },
            390.0,
            844.0,
        );
        assert_inset(&rt_drawer, "rail-drawer", "nav-Projects");
    }

    #[test]
    fn webhook_settings_and_diagnostics_transport_render() {
        let rt_gen = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::General,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_gen.find("field-general.trigger_mode").is_some());
        assert!(rt_gen.find("general-trigger-mode").is_some());

        let rt_gh = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Github,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_gh.find("field-github.webhook_secret").is_some());
        assert!(rt_gh.find("github-webhook-info").is_some());

        let rt_proj = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Projects,
                    },
                )
            },
            1180.0,
            760.0,
        );
        assert!(rt_proj.find("field-project[0].trigger_mode").is_some());
        assert!(rt_proj.find("project-trigger-mode-0").is_some());

        // Diagnostics shows Transport (WS live vs HTTP fallback) and GitHub Webhook status.
        let rt_diag_ws = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Diagnostics,
                    },
                );
                s.ws_connected = true;
            },
            1180.0,
            760.0,
        );
        assert!(rt_diag_ws.find("diagnostics-transport").is_some());
        assert!(rt_diag_ws.find("diagnostics-webhook").is_some());
        let diag_texts: Vec<String> = rt_diag_ws
            .elements()
            .into_iter()
            .filter_map(|e| e.text)
            .collect();
        assert!(diag_texts.iter().any(|t| t == "WebSocket (live)"));

        let rt_diag_http = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Diagnostics,
                    },
                );
                s.ws_connected = false;
            },
            1180.0,
            760.0,
        );
        let diag_texts_http: Vec<String> = rt_diag_http
            .elements()
            .into_iter()
            .filter_map(|e| e.text)
            .collect();
        assert!(diag_texts_http.iter().any(|t| t == "HTTP fallback"));
    }

    #[test]
    fn settings_projects_e2e_scroll_targets_stay_in_view() {
        let assert_in_viewport = |rt: &Runtime<State>, key: &str, label: &str| {
            let els = rt.elements();
            let body = els
                .iter()
                .find(|e| e.key.as_deref() == Some("settings-body") && !e.exiting)
                .expect("settings-body");
            let target = els
                .iter()
                .find(|e| e.key.as_deref() == Some(key) && !e.exiting)
                .unwrap_or_else(|| panic!("{label}: missing {key}"));
            let cy = target.rect[1] + target.rect[3] * 0.5;
            assert!(
                cy >= body.rect[1] + 2.0 && cy <= body.rect[1] + body.rect[3] - 2.0,
                "{label}: {key} center y={cy:.1} outside settings-body [{:.1}, {:.1}] (target rect={:?})",
                body.rect[1],
                body.rect[1] + body.rect[3],
                target.rect
            );
        };

        // T6 desktop 1280x840: wheel -350 -> field-project[0].env/control; wheel -800 on new project -> add-shell-ci
        let mut rt_env = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Projects,
                    },
                )
            },
            1280.0,
            840.0,
        );
        let (bx, by) = rt_env.find("settings-body").expect("settings-body");
        rt_env.wheel_xy(bx, by, 0.0, -350.0);
        for _ in 0..20 {
            rt_env.frame(0.016);
            rt_env.settle_all();
        }
        assert_in_viewport(&rt_env, "field-project[0].env", "T6 env 1280x840");

        let mut rt_new = setup_runtime(
            |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::Projects,
                    },
                );
                let mut tx = rune::Tx::default();
                s.add_project(&mut tx);
            },
            1280.0,
            840.0,
        );
        let (bx, by) = rt_new.find("settings-body").expect("settings-body");
        rt_new.wheel_xy(bx, by, 0.0, -800.0);
        for _ in 0..20 {
            rt_new.frame(0.016);
            rt_new.settle_all();
        }
        assert_in_viewport(&rt_new, "add-shell-ci", "T6 add-shell-ci 1280x840");

        // Responsive layouts: step-row-ci-0 visible after scroll across all 4 viewports.
        for (w, h, wheels) in [
            (390.0, 844.0, vec![-800.0, -800.0]),
            (360.0, 780.0, vec![-800.0, -800.0]),
            (768.0, 1024.0, vec![-900.0, -900.0]),
            (1180.0, 760.0, vec![-950.0]),
        ] {
            let mut rt = setup_runtime(
                |s| {
                    crate::demo::load(
                        s,
                        View::Settings {
                            page: SettingsPage::Projects,
                        },
                    )
                },
                w,
                h,
            );
            let (bx, by) = rt.find("settings-body").expect("settings-body");
            for dy in wheels {
                rt.wheel_xy(bx, by, 0.0, dy);
                for _ in 0..20 {
                    rt.frame(0.016);
                    rt.settle_all();
                }
            }
            assert_in_viewport(&rt, "step-row-ci-0", &format!("resp {w}x{h}"));
        }
    }
}
