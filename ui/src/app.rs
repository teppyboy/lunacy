//! The window: the titlebar, the navigation rail, the connection banner, the status bar and the
//! toast layer. Screens fill the content area; this file only decides which one shows.

use rune::{
    col, el, icon, row, text, Align, AppSpec, Color, Cx, El, Key, KeyEvent, Mods, ScrollSpec,
    TextEdit, Tx,
};
use rune_kit::{glyphs, Kit};

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
    State::new(crate::state::conn_from_env())
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
    ("offline", crate::demo::offline),
    ("unauthorized", crate::demo::unauthorized),
    ("toast", crate::demo::with_toast),
];

// ---------------------------------------------------------------------------------------------
// Keys

/// Global keys the focused control didn't take.
fn on_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
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
                tx.instant();
                return true;
            }
            if s.editor.step.is_some() {
                s.editor.step = None;
                tx.instant();
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
    let k = theme::kit_for(dark, compact);
    let scale = cx.env().scale;
    let hair = theme::hair(scale);
    let safe = cx.safe_area().map(|v| (v / zoom).max(0.0));

    let content = if compact {
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(screen(s, &k, cx, hair, compact))
    } else {
        row()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(rail(s, &k, hair, compact))
            .child(
                col()
                    .grow(1.0)
                    .min_h(0.0)
                    .child(screen(s, &k, cx, hair, compact)),
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
        .child(content);

    let drawer = (compact && s.drawer_open).then(|| drawer_overlay(s, &k, hair, layout_w, safe));
    // Drawer sits below toasts so error notices remain readable if the drawer opens.
    let toasts = toast_layer(s, &k, compact, layout_w, safe);
    rune_kit::root(body, drawer.into_iter().chain(toasts))
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
    let mut bar = row()
        .key("titlebar")
        .w_full()
        .h(if compact { 52.0 } else { 46.0 })
        .pl(inset.leading.max(if compact { 8.0 } else { 14.0 }))
        .pr(inset.trailing.max(if compact { 8.0 } else { 14.0 }))
        .gap(if compact { 6.0 } else { 10.0 })
        .center_y()
        .fill(k.c.page);
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
                    .center()
                    .cursor(rune::Cursor::Pointer)
                    .child(widgets::hamburger(k.c.icon_base))
                    .on_press_tx(|s: &mut State, tx: &mut Tx| {
                        s.drawer_open = !s.drawer_open;
                        tx.instant();
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
    .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
}

/// The connection chip: dot and label on desktop; compact 44×44 dot pill on phones.
fn connection_chip(s: &State, k: &Kit, compact: bool) -> El {
    let (ink, label) = match &s.link {
        Link::Connecting => (k.c.text_muted, "Connecting".to_string()),
        Link::Ok => (theme::status(k).success, "Connected".to_string()),
        Link::Down { .. } => (theme::status(k).failure, "Offline".to_string()),
        Link::Unauthorized => (theme::status(k).error, "Unauthorized".to_string()),
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
                    .child(widgets::dot(ink, 8.0)),
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
        .child(widgets::dot(ink, 7.0))
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
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .items_start()
                        .child(widgets::dot(ink, 8.0).mt(5.0))
                        .child(
                            col()
                                .grow(1.0)
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
            .child(widgets::dot(ink, 8.0))
            .child(text(title).font(k.ty.small_medium).color(ink).nowrap())
            .child(
                text(body)
                    .key("banner-body")
                    .font(k.ty.small)
                    .color(ink)
                    .nowrap()
                    .truncate()
                    .grow(1.0),
            )
            .child(conn_btn)
            .child(retry_btn),
    )
}

/// The left rail: Projects, Settings, and the projects themselves.
fn rail(s: &State, k: &Kit, hair: f32, compact: bool) -> El {
    let row_h = if compact { 44.0 } else { 30.0 };
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
            let st = theme::status(k).for_project(project.state());
            let name = project.name.clone();
            let selected = matches!(&s.view, View::Project { name: open } if open == &name);
            list = list.child(
                row()
                    .key(format!("rail-{}", project.name))
                    .w_full()
                    .h(row_h)
                    .px(14.0)
                    .gap(8.0)
                    .center_y()
                    .fill(if selected {
                        k.c.layer_03
                    } else {
                        Color::TRANSPARENT
                    })
                    .hover(k.c.hover)
                    .child(widgets::dot(st, 7.0))
                    .child(
                        text(project.name.clone())
                            .font(if compact { k.ty.ui } else { k.ty.small })
                            .color(if selected {
                                k.c.text_base
                            } else {
                                k.c.text_muted
                            })
                            .nowrap()
                            .truncate(),
                    )
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                        s.open_project(&name.clone(), tx)
                    })
                    .cursor(rune::Cursor::Pointer),
            );
        }
    }

    col()
        .key(if compact { "rail-drawer-body" } else { "rail" })
        .w(if compact { 272.0 } else { 196.0 })
        .h_full()
        .shrink()
        .gap(0.0)
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, 0.0, hair], k.c.border_muted)
        .child(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec::default())
                .child(list),
        )
        .child(
            col()
                .w_full()
                .px(14.0)
                .py(10.0)
                .gap(2.0)
                .border_sides([hair, 0.0, 0.0, 0.0], k.c.border_muted)
                .child(
                    // Two lines rather than one truncated "daemon 0....": the
                    // rail is 196px, so the address wraps under the version.
                    text(format!(
                        "daemon {}\n{}",
                        if s.version.is_empty() {
                            "—".into()
                        } else {
                            s.version.clone()
                        },
                        s.server_display()
                    ))
                    .font(k.ty.small)
                    .color(k.c.text_faint)
                    .wrap(),
                ),
        )
}

fn drawer_overlay(s: &State, k: &Kit, hair: f32, layout_w: f32, safe: [f32; 4]) -> El {
    let drawer_w = (layout_w - safe[3] - 48.0).clamp(220.0, 272.0);
    let drawer_head = row()
        .w_full()
        .h(52.0)
        .px(14.0)
        .gap(8.0)
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
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
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    s.drawer_open = false;
                    tx.instant();
                })
                .build(k)
                .key("drawer-close"),
            true,
        ));
    let drawer = col()
        .key("rail-drawer")
        .absolute(0.0, 0.0)
        .w(drawer_w + safe[3])
        .h_full()
        .pt(safe[0])
        .pb(safe[2])
        .pl(safe[3])
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, 0.0, hair], k.c.border_base)
        .child(drawer_head)
        .child(rail(s, k, hair, true).w_full().grow(1.0).min_h(0.0));
    el().key("drawer-layer")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .child(
            el().key("drawer-scrim")
                .absolute(0.0, 0.0)
                .w_full()
                .h_full()
                .fill(Color::BLACK.alpha(0.45))
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    s.drawer_open = false;
                    tx.instant();
                }),
        )
        .child(drawer)
}

fn nav_row(k: &Kit, label: &str, selected: bool, h: f32, press: fn(&mut State, &mut Tx)) -> El {
    row()
        .key(format!("nav-{label}"))
        .w_full()
        .h(h)
        .px(14.0)
        .center_y()
        .fill(if selected {
            k.c.layer_03
        } else {
            Color::TRANSPARENT
        })
        .hover(k.c.hover)
        .child(text(label).font(k.ty.ui).color(if selected {
            k.c.text_base
        } else {
            k.c.text_muted
        }))
        .on_press_tx(press)
        .cursor(rune::Cursor::Pointer)
}

/// The screen body for the current view.
fn screen(s: &State, k: &Kit, cx: &mut Cx<State>, hair: f32, compact: bool) -> El {
    match &s.view {
        View::Projects => views::projects::view(s, k, cx, hair, compact),
        View::Project { name } => views::project::view(s, k, name, cx, hair, compact),
        View::Run { id } => views::run::view(s, k, id, cx, hair, compact),
        View::Settings { page } => views::settings::view(s, k, *page, cx, hair, compact),
    }
}

/// The toast layer.
fn toast_layer(s: &State, k: &Kit, compact: bool, layout_w: f32, safe: [f32; 4]) -> Option<El> {
    if s.toasts.is_empty() {
        return None;
    }
    let toast_w = if compact {
        (layout_w - safe[1] - safe[3] - 24.0).clamp(240.0, 360.0)
    } else {
        360.0
    };
    let mut stack = col().gap(8.0).items_end();
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
                .child(widgets::dot(glyph, 8.0).mt(5.0))
                .child(
                    col()
                        .grow(1.0)
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
    Some(
        el().key("toast-layer")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .pr(if compact { 12.0 } else { 18.0 } + safe[1])
            .pb(if compact { 12.0 } else { 18.0 } + safe[2])
            .pl(if compact { 12.0 + safe[3] } else { 0.0 })
            .justify_end()
            .items_end()
            .child(stack),
    )
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
            rt.frame(0.016);
            rt.settle_all();
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
        for (w, h) in [(390.0, 844.0), (360.0, 780.0), (768.0, 1024.0)] {
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
                crate::demo::load(s, View::Run { id: rid })
            });
            assert_responsive_scene("settings-general", w, h, |s| {
                crate::demo::load(
                    s,
                    View::Settings {
                        page: SettingsPage::General,
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
                )
            });
            assert_responsive_scene("settings-step-editor", w, h, |s| {
                crate::demo::editor_step(s);
            });
            assert_responsive_scene("unauthorized", w, h, |s| {
                crate::demo::unauthorized(s);
            });
            assert_responsive_scene("drawer-open", w, h, |s| {
                crate::demo::load(s, View::Projects);
                s.drawer_open = true;
            });
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
}
