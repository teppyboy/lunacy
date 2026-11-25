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
        min_size: Some((860.0, 560.0)),
        ..AppSpec::new("lunacy", (1180.0, 760.0), init, view)
    })
    .titlebar(rune::Titlebar::new(38.0).controls(true))
    .chrome(rune::Chrome::DEFAULT);

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
    let dark = cx.system_dark();
    let k = theme::kit(dark);
    let scale = cx.env().scale;
    let hair = theme::hair(scale);

    let overlays = Vec::new();
    let body = col()
        .w_full()
        .h_full()
        .fill(k.c.page)
        .child(titlebar(s, &k, cx, hair))
        .children(banner(s, &k, hair))
        .child(
            row()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(rail(s, &k, hair))
                .child(col().grow(1.0).min_h(0.0).child(screen(s, &k, cx, hair))),
        );

    // Toasts above everything, bottom-right.
    let toasts = toast_layer(s, &k);
    rune_kit::root(body, toasts.into_iter().chain(overlays))
}

/// The window's title row: the app name, the current screen's title, and the connection state.
fn titlebar(s: &State, k: &Kit, cx: &Cx<State>, hair: f32) -> El {
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
    row()
        .key("titlebar")
        .w_full()
        .h(46.0)
        .pl(inset.leading.max(14.0))
        .pr(inset.trailing.max(14.0))
        .gap(10.0)
        .center_y()
        .fill(k.c.page)
        .window_drag()
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
                .font(k.ty.ui)
                .color(k.c.text_muted)
                .nowrap()
                .truncate()
                .grow(1.0),
        )
        .child(
            row()
                .key("titlebar-actions")
                .gap(6.0)
                .center_y()
                .child(connection_chip(s, k))
                .child(
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
                ),
        )
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
}

/// The connection chip: a dot and a word, so the state reads at a glance.
fn connection_chip(s: &State, k: &Kit) -> El {
    let (ink, label) = match &s.link {
        Link::Connecting => (k.c.text_muted, "Connecting".to_string()),
        Link::Ok => (theme::status(k).success, "Connected".to_string()),
        Link::Down { .. } => (theme::status(k).failure, "Offline".to_string()),
        Link::Unauthorized => (theme::status(k).error, "Unauthorized".to_string()),
    };
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
fn banner(s: &State, k: &Kit, hair: f32) -> Option<El> {
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
                    .font(k.ty.small)
                    .color(ink)
                    .nowrap()
                    .truncate()
                    .grow(1.0),
            )
            .child(
                rune_kit::Button::new("Connection")
                    .variant(rune_kit::ButtonVariant::Ghost)
                    .control_size(rune_kit::ControlSize::Small)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| {
                        s.open_settings(SettingsPage::Connection, tx)
                    })
                    .build(k)
                    .key("banner-connection"),
            )
            .child(
                rune_kit::Button::new("Retry")
                    .variant(rune_kit::ButtonVariant::Outline)
                    .control_size(rune_kit::ControlSize::Small)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| s.reconnect(tx))
                    .build(k)
                    .key("banner-retry"),
            ),
    )
}

/// The left rail: Projects, Settings, and the projects themselves.
fn rail(s: &State, k: &Kit, hair: f32) -> El {
    let mut list = col().w_full().gap(1.0).pt(8.0).pb(8.0);
    list = list.child(nav_row(
        k,
        "Projects",
        s.view == View::Projects || matches!(s.view, View::Project { .. }),
        |s: &mut State, tx: &mut Tx| s.open_projects(tx),
    ));
    list = list.child(nav_row(
        k,
        "Settings",
        matches!(s.view, View::Settings { .. }),
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
                    .h(30.0)
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
                            .font(k.ty.small)
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
        .key("rail")
        .w(196.0)
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

fn nav_row(k: &Kit, label: &str, selected: bool, press: fn(&mut State, &mut Tx)) -> El {
    row()
        .key(format!("nav-{label}"))
        .w_full()
        .h(30.0)
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
fn screen(s: &State, k: &Kit, cx: &mut Cx<State>, hair: f32) -> El {
    match &s.view {
        View::Projects => views::projects::view(s, k, cx, hair),
        View::Project { name } => views::project::view(s, k, name, cx, hair),
        View::Run { id } => views::run::view(s, k, id, cx, hair),
        View::Settings { page } => views::settings::view(s, k, *page, cx, hair),
    }
}

/// The toast layer.
fn toast_layer(s: &State, k: &Kit) -> Option<El> {
    if s.toasts.is_empty() {
        return None;
    }
    let mut stack = col().gap(8.0).items_end();
    for toast in &s.toasts {
        let (_ink, glyph) = match toast.kind {
            ToastKind::Info => (k.c.text_accent, k.c.text_accent),
            ToastKind::Success => (theme::status(k).success, theme::status(k).success),
            ToastKind::Error => (theme::status(k).failure, theme::status(k).failure),
        };
        let id = toast.id;
        stack = stack.child(
            row()
                .key(format!("toast-{id}"))
                .w(360.0)
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
                .child(
                    el().key(format!("toast-close-{id}"))
                        .size(18.0, 18.0)
                        .center()
                        .radius(k.radius.xs)
                        .hover(k.c.hover)
                        .child(icon(&glyphs::CLOSE, 12.0).color(k.c.icon_muted))
                        .on_press(move |s: &mut State| s.dismiss_toast(id))
                        .cursor(rune::Cursor::Pointer),
                ),
        );
    }
    Some(
        el().key("toast-layer")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .pr(18.0)
            .pb(18.0)
            .justify_end()
            .items_end()
            .child(stack),
    )
}

// `Align` and `Mods` are used by screens; keeping the imports here documents the key contract.
#[allow(unused_imports)]
use {Align as _Align, Mods as _Mods, TextEdit as _TextEdit};
