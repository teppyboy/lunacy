//! The settings shell: a sidebar of pages, the page body, and the save bar.
//!
//! The daemon's config is edited as the JSON it already is (`Editor::draft`), so unknown keys
//! survive a save and a rejected field is addressed by the daemon's own path.

use rune::{col, el, row, text, Cx, El, ScrollSpec, TextEdit, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Field, Kit, TextArea, TextInput};

use crate::state::{SettingsPage, State};
use crate::theme;
use crate::views::{diagnostics, steps};
use crate::widgets;

pub fn view(
    s: &State,
    k: &Kit,
    page: SettingsPage,
    cx: &mut Cx<State>,
    hair: f32,
    compact: bool,
) -> El {
    let content = col()
        .key("settings-body")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .h_full()
        .scroll(ScrollSpec::default())
        .child(match page {
            SettingsPage::Diagnostics => diagnostics::view(s, k, cx, compact),
            SettingsPage::Connection => connection_page(s, k, compact),
            SettingsPage::Projects if s.editor.project.is_some() => {
                projects_detail(s, k, cx, compact)
            }
            SettingsPage::Projects => projects_list(s, k, compact),
            other => config_page(s, k, other, cx, compact),
        });

    let body = if compact {
        let mut strip = row().gap(6.0).center_y();
        for p in SettingsPage::CONFIG
            .into_iter()
            .chain([SettingsPage::Diagnostics, SettingsPage::Connection])
        {
            strip = strip.child(page_chip(k, s, p));
        }
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(
                row()
                    .key("settings-nav-strip")
                    .w_full()
                    .px(14.0)
                    .py(8.0)
                    .fill(k.c.layer_01)
                    .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
                    .scroll_x()
                    .child(strip),
            )
            .child(content)
    } else {
        let mut pages = col()
            .w(212.0)
            .shrink()
            .h_full()
            .gap(1.0)
            .py(10.0)
            .fill(k.c.layer_01)
            .border_sides([0.0, 0.0, 0.0, hair], k.c.border_muted);
        pages = pages.child(
            row()
                .w_full()
                .px(16.0)
                .pb(8.0)
                .child(widgets::caption(k, "CONFIG")),
        );
        for p in SettingsPage::CONFIG {
            pages = pages.child(page_row(k, s, p));
        }
        pages = pages.child(
            row()
                .w_full()
                .px(16.0)
                .pt(14.0)
                .pb(8.0)
                .child(widgets::caption(k, "THIS WINDOW")),
        );
        for p in [SettingsPage::Diagnostics, SettingsPage::Connection] {
            pages = pages.child(page_row(k, s, p));
        }
        row()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(pages)
            .child(content)
    };

    col()
        .key("settings")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(save_bar(s, k, page, hair, compact))
        .child(body)
}

fn page_chip(k: &Kit, s: &State, page: SettingsPage) -> El {
    let selected = s.editor.page == page;
    el().key(format!("settings-nav-{}", page.title()))
        .role("tab")
        .label(page.title())
        .focus_selected(selected)
        .h(44.0)
        .min_w(44.0)
        .px(14.0)
        .center()
        .radius(k.radius.md)
        .fill(if selected { k.c.layer_04 } else { k.c.layer_02 })
        .border(
            1.0,
            if selected {
                k.c.border_strong
            } else {
                k.c.border_muted
            },
        )
        .hover(k.c.hover)
        .child(
            text(page.title())
                .font(k.ty.small_medium)
                .color(if selected {
                    k.c.text_base
                } else {
                    k.c.text_muted
                })
                .nowrap(),
        )
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_settings(page, tx))
        .cursor(rune::Cursor::Pointer)
}

fn page_row(k: &Kit, s: &State, page: SettingsPage) -> El {
    let selected = s.editor.page == page;
    row()
        .key(format!("settings-nav-{}", page.title()))
        .w_full()
        .h(30.0)
        .px(16.0)
        .center_y()
        .fill(if selected {
            k.c.layer_03
        } else {
            rune::Color::TRANSPARENT
        })
        .hover(k.c.hover)
        .child(text(page.title()).font(k.ty.ui).color(if selected {
            k.c.text_base
        } else {
            k.c.text_muted
        }))
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_settings(page, tx))
        .cursor(rune::Cursor::Pointer)
}

/// The header with dirty state, Save, Discard and the ⌘S hint.
fn save_bar(s: &State, k: &Kit, page: SettingsPage, hair: f32, compact: bool) -> El {
    let e = &s.editor;
    let local = matches!(page, SettingsPage::Connection | SettingsPage::Diagnostics);
    let dirty = e.dirty();

    let title_row = row()
        .gap(8.0)
        .center_y()
        .child(text(page.title()).font(k.ty.ui_medium).color(k.c.text_base))
        .children(dirty.then(|| widgets::pill(k, "Unsaved", theme::status(k).error)));

    let discard_btn = Button::new("Discard")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .disabled(!dirty)
        .on_press_tx(|s: &mut State, tx: &mut Tx| s.discard(tx))
        .build(k)
        .key("settings-discard");

    let mut save_builder = Button::new(if e.saving { "Saving…" } else { "Save" })
        .variant(ButtonVariant::Submit)
        .control_size(ControlSize::Small)
        .disabled(!dirty || e.saving || !e.loaded)
        .shortcut("cmd+s");
    if !compact {
        save_builder = save_builder.show_shortcut();
    }
    let save_btn = save_builder
        .on_press_tx(|s: &mut State, _tx: &mut Tx| s.save())
        .build(k)
        .key("settings-save");

    if compact {
        let mut bar = col()
            .key("settings-bar")
            .w_full()
            .px(14.0)
            .py(12.0)
            .gap(10.0)
            .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
            .child(
                col().w_full().gap(2.0).child(title_row).child(
                    text(page.blurb())
                        .font(k.ty.small)
                        .color(k.c.text_muted)
                        .wrap(),
                ),
            );
        if !local {
            bar = bar.child(
                row()
                    .w_full()
                    .gap(8.0)
                    .flex_wrap()
                    .child(discard_btn.grow(1.0))
                    .child(save_btn.grow(1.0)),
            );
        }
        return bar;
    }

    let mut bar = row()
        .key("settings-bar")
        .w_full()
        .px(22.0)
        .py(12.0)
        .gap(10.0)
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
        .child(
            col().gap(1.0).grow(1.0).child(title_row).child(
                text(page.blurb())
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .nowrap()
                    .truncate(),
            ),
        );

    if !local {
        bar = bar.child(discard_btn).child(save_btn);
    }
    bar
}

// ---------------------------------------------------------------------------------------------
// Pages

fn config_page(s: &State, k: &Kit, page: SettingsPage, cx: &mut Cx<State>, compact: bool) -> El {
    let e = &s.editor;
    if !e.loaded {
        return widgets::empty_state(
            k,
            "Reading the config…",
            "The daemon has not answered yet. Check the connection in the banner.",
        );
    }
    let _ = cx;
    let _ = TextEdit::new("");
    let mut body = col()
        .key("config-page")
        .w_full()
        .px(if compact { 14.0 } else { 22.0 })
        .py(if compact { 14.0 } else { 18.0 })
        .gap(22.0);
    if let Some(field) = &e.field {
        let message = e.error.clone().unwrap_or_default();
        body = body.child(widgets::notice(
            k,
            theme::status(k).failure,
            &format!("{field}: {message}"),
        ));
    }
    body = body.child(match page {
        SettingsPage::General => general(s, k),
        SettingsPage::Server => server(s, k),
        SettingsPage::Github => github(s, k, compact),
        SettingsPage::Discord => discord(s, k, compact),
        SettingsPage::Act => act(s, k),
        _ => col(),
    });
    body
}

fn general(s: &State, k: &Kit) -> El {
    let e = &s.editor;
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(
        k,
        "General",
        Some("Where runs are recorded and how often upstream is checked."),
    ));
    c = c.child(text_field(
        s,
        k,
        "general.state_dir",
        "State directory",
        Some("Run history and logs live here. Empty uses the platform's data directory."),
    ));
    c = c.child(u64_field(
        s,
        k,
        "general.poll_interval",
        "Poll interval (seconds)",
        Some("How often each project's remote is fetched. A project may override it."),
        true,
    ));
    c = c.child(argv_field(
        s,
        k,
        "general.shell",
        "Default shell",
        Some("Command used to run shell steps, e.g. `bash -lc`. Empty uses the platform default."),
    ));
    let _ = e;
    c
}

fn server(s: &State, k: &Kit) -> El {
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(
        k,
        "Server",
        Some("The HTTP API this window talks to."),
    ));
    c = c.child(text_field(
        s,
        k,
        "server.listen",
        "Listen address",
        Some("`127.0.0.1:7878`. Omit the section entirely to disable the API."),
    ));
    c = c.child(secret_field(
        s,
        k,
        "server.token",
        "Bearer token",
        Some("If set, every /api request needs `Authorization: Bearer <token>`."),
    ));
    c = c.child(text_field(
        s,
        k,
        "server.public_url",
        "Public URL",
        Some("How this daemon is reached from elsewhere. Used for links in GitHub checks."),
    ));
    c = c.child(text_field(
        s,
        k,
        "server.web_root",
        "Web root",
        Some("Static files served at `/`, e.g. `ui/dist` from `build-web.sh`. Empty disables it."),
    ));
    c
}

fn github(s: &State, k: &Kit, compact: bool) -> El {
    let e = &s.editor;
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(k, "GitHub", Some("How a run is reported back to GitHub. A token is enough for commit statuses; an App gives check runs with per-step output.")));

    let mode = e.s("github.mode");
    let mode_index = if mode == "checks" {
        Some(0)
    } else if mode == "status" {
        Some(1)
    } else {
        None
    };
    c = c.child(widgets::labelled(
        k,
        "Reporting",
        Some("Check runs need an App; commit statuses work with any token that can write them."),
        widgets::segmented(
            k,
            compact,
            &["Checks (App)", "Status (token)"],
            mode_index,
            |s: &mut State, picked: usize, tx: &mut Tx| {
                s.editor
                    .set_str("github.mode", if picked == 0 { "checks" } else { "status" });
                tx.instant();
            },
        )
        .key("github-mode"),
    ));
    c = c.child(secret_field(
        s,
        k,
        "github.token",
        "Token",
        Some("A PAT with `repo` scope for commit statuses. Masked once stored."),
    ));

    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(k, "GitHub App", Some("Optional. The App's installation writes check runs, which the UI links as \"View on GitHub\".")));
    c = c.child(u64_field(s, k, "github.app.app_id", "App ID", None, false));
    c = c.child(pem_secret_field(
        s,
        k,
        "github.app.private_key",
        "Private key",
        Some("The multi-line PEM from the App's settings page."),
    ));
    c = c.child(u64_field(
        s,
        k,
        "github.app.installation_id",
        "Installation ID",
        None,
        false,
    ));
    c = c.child(secret_field(
        s,
        k,
        "github.app.webhook_secret",
        "Webhook secret",
        Some("Verifies `POST /api/github/webhook`; a re-requested check reruns that commit."),
    ));
    c
}

fn discord(s: &State, k: &Kit, compact: bool) -> El {
    let e = &s.editor;
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(
        k,
        "Discord",
        Some("Post a message when a pipeline starts and when it finishes."),
    ));
    c = c.child(secret_field(
        s,
        k,
        "discord.webhook_url",
        "Webhook URL",
        Some("A channel webhook. Masked once stored; projects can override it."),
    ));
    c = c.child(toggle_row(
        k,
        compact,
        "discord-notify-on-start",
        "Notify on start",
        Some("Also post when a run begins, not only when it ends."),
        crate::paths::get_bool(&e.draft, "discord.notify_on_start"),
        |s: &mut State, tx: &mut Tx| {
            let on = crate::paths::get_bool(&s.editor.draft, "discord.notify_on_start");
            crate::paths::set_bool(&mut s.editor.draft, "discord.notify_on_start", !on);
            tx.instant();
        },
    ));
    c
}

fn act(s: &State, k: &Kit) -> El {
    let e = &s.editor;
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(k, "act", Some("The runner for GitHub Actions steps: how it is invoked and which image each `runs-on` maps to.")));
    c = c.child(argv_field(
        s,
        k,
        "act.command",
        "Command",
        Some("Usually just `act`. Use `wsl.exe -e act` on Windows to run it inside WSL."),
    ));
    c = c.child(list_field(
        s,
        k,
        "act.args",
        "Extra arguments",
        Some("One per line, e.g. `--pull=false`."),
    ));
    c = c.child(table_field(
        s,
        k,
        "act.platforms",
        "Platforms",
        Some("`runs-on=image`, one per line. Unlisted labels use act's own default image."),
    ));
    c = c.child(text_field(
        s,
        k,
        "act.lock_file",
        "Lock file",
        Some(
            "The `flock` every act run takes, shared with scripts that call `flock` on it. \
             Defaults to `act.lock` in the state dir; `~/` is expanded.",
        ),
    ));
    let _ = e;
    c
}

/// A text field bound to a config path, with inline error and help.
pub(crate) fn text_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    let seeded = crate::paths::get_str(&s.editor.draft, path);
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let invalid = s.editor.invalid(path);
    let error = s.editor.error_for(path).map(str::to_string);
    let owner = path.to_string();
    let field = path.to_string();
    let input = TextInput::new(&edit)
        .label(label)
        .placeholder(label)
        .width(None)
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_str(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            let optional = is_optional(&owner);
            s.editor.set_str_mode(&owner, &text, optional);
            let now_seeded = crate::paths::get_str(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
            tx.instant();
        })
        .build(k);
    let mut f = Field::new().label(label).control(input.key("control"));
    if let Some(help) = help {
        f = f.prefix(help);
    }
    if let Some(error) = error {
        f = f.suffix(error);
    }
    f.build(k).key(format!("field-{path}"))
}

/// An unsigned integer field bound to a config path (`u64` or `Option<u64>`).
pub(crate) fn u64_field(
    s: &State,
    k: &Kit,
    path: &str,
    label: &str,
    help: Option<&str>,
    required_zero: bool,
) -> El {
    let seeded = crate::paths::get_str(&s.editor.draft, path);
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let invalid = s.editor.invalid(path);
    let error = s.editor.error_for(path).map(str::to_string);
    let owner = path.to_string();
    let field = path.to_string();
    let input = TextInput::new(&edit)
        .label(label)
        .placeholder(label)
        .width(None)
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_str(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                let n = required_zero.then_some(0);
                crate::paths::set_u64(&mut s.editor.draft, &owner, n);
            } else if let Ok(n) = trimmed.parse::<u64>() {
                crate::paths::set_u64(&mut s.editor.draft, &owner, Some(n));
            } else {
                crate::paths::set(
                    &mut s.editor.draft,
                    &owner,
                    serde_json::Value::String(trimmed.to_string()),
                );
            }
            prune_github_if_empty(&mut s.editor.draft, &owner);
            let now_seeded = crate::paths::get_str(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
            tx.instant();
        })
        .build(k);
    let mut f = Field::new().label(label).control(input.key("control"));
    if let Some(help) = help {
        f = f.prefix(help);
    }
    if let Some(error) = error {
        f = f.suffix(error);
    }
    f.build(k).key(format!("field-{path}"))
}

/// An argv array field (`Option<Vec<String>>`), formatted with `join_argv` and parsed with `split_argv`.
pub(crate) fn argv_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    let seeded = crate::config::join_argv(&argv_vec(&s.editor.draft, path));
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let invalid = s.editor.invalid(path);
    let error = s.editor.error_for(path).map(str::to_string);
    let owner = path.to_string();
    let field = path.to_string();
    let input = TextInput::new(&edit)
        .label(label)
        .placeholder(label)
        .width(None)
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::config::join_argv(&argv_vec(&s.editor.draft, &field));
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            let argv = crate::config::split_argv(&text);
            if argv.is_empty() {
                crate::paths::set(&mut s.editor.draft, &owner, serde_json::Value::Null);
            } else {
                let items = argv.into_iter().map(serde_json::Value::String).collect();
                crate::paths::set(&mut s.editor.draft, &owner, serde_json::Value::Array(items));
            }
            let now_seeded = crate::config::join_argv(&argv_vec(&s.editor.draft, &owner));
            s.editor.sync_seeded(&owner, now_seeded);
            tx.instant();
        })
        .build(k);
    let mut f = Field::new().label(label).control(input.key("control"));
    if let Some(help) = help {
        f = f.prefix(help);
    }
    if let Some(error) = error {
        f = f.suffix(error);
    }
    f.build(k).key(format!("field-{path}"))
}

fn argv_vec(v: &serde_json::Value, path: &str) -> Vec<String> {
    crate::paths::array(v, path)
        .iter()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// A line-per-item `Vec<String>` list bound to a config path.
pub(crate) fn list_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    let seeded = crate::paths::get_list_text(&s.editor.draft, path);
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let invalid = s.editor.invalid(path);
    let error = s.editor.error_for(path).map(str::to_string);
    let owner = path.to_string();
    let field = path.to_string();
    let input = TextArea::new(&edit)
        .label(label)
        .width(None)
        .rows(2)
        .auto_grow(crate::theme::area_min_rows(k), 8)
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_list_text(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            crate::paths::set_list(&mut s.editor.draft, &owner, &text);
            let now_seeded = crate::paths::get_list_text(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
            tx.instant();
        })
        .build(k);
    let mut f = Field::new().label(label).control(input.key("control"));
    if let Some(help) = help {
        f = f.prefix(help);
    }
    if let Some(error) = error {
        f = f.suffix(error);
    }
    f.build(k).key(format!("field-{path}"))
}

/// A masked secret field: `********` means "keep what is stored".
fn secret_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    let seeded = crate::paths::get_str(&s.editor.draft, path);
    let stored = seeded == crate::config::MASK;
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let invalid = s.editor.invalid(path);
    let error = s.editor.error_for(path).map(str::to_string);
    let owner = path.to_string();
    let field = path.to_string();
    let input = TextInput::new(&edit)
        .label(label)
        .placeholder(if stored {
            "Stored — leave to keep"
        } else {
            label
        })
        .width(None)
        .secure()
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_str(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            s.editor.set_str(&owner, &text);
            prune_github_if_empty(&mut s.editor.draft, &owner);
            let now_seeded = crate::paths::get_str(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
            tx.instant();
        })
        .build(k);
    secret_field_wrap(k, path, label, help, stored, error, input)
}

/// Multi-line secret area (for PEM keys where newlines must survive editing).
fn pem_secret_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    let seeded = crate::paths::get_str(&s.editor.draft, path);
    let stored = seeded == crate::config::MASK;
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let invalid = s.editor.invalid(path);
    let error = s.editor.error_for(path).map(str::to_string);
    let owner = path.to_string();
    let field = path.to_string();
    let input = TextArea::new(&edit)
        .label(label)
        .width(None)
        .rows(3)
        .auto_grow(crate::theme::area_min_rows(k), 10)
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_str(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            s.editor.set_str(&owner, &text);
            prune_github_if_empty(&mut s.editor.draft, &owner);
            let now_seeded = crate::paths::get_str(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
            tx.instant();
        })
        .build(k);
    secret_field_wrap(k, path, label, help, stored, error, input)
}

fn secret_field_wrap(
    k: &Kit,
    path: &str,
    label: &str,
    help: Option<&str>,
    stored: bool,
    error: Option<String>,
    input: El,
) -> El {
    let mut f = Field::new().label(label).control(input.key("control"));
    let note = match (stored, help) {
        (true, Some(h)) => format!(
            "{h} A stored secret shows as `********`; leave it to keep it, or type a new one."
        ),
        (true, None) => {
            "A stored secret shows as `********`; leave it to keep it, or type a new one."
                .to_string()
        }
        (false, Some(h)) => h.to_string(),
        (false, None) => String::new(),
    };
    if !note.is_empty() {
        f = f.prefix(note);
    }
    if let Some(error) = error {
        f = f.suffix(error);
    }
    f.build(k).key(format!("field-{path}"))
}

fn prune_github_if_empty(draft: &mut serde_json::Value, path: &str) {
    if path.starts_with("github.app.") {
        crate::paths::prune_empty_object(draft, "github.app");
    }
    if path.starts_with("github.") {
        crate::paths::prune_empty_object(draft, "github");
    }
}

/// Optional config fields: an empty value is removed rather than stored as `""`.
fn is_optional(path: &str) -> bool {
    !matches!(path, "server.listen")
        && !matches!(crate::paths::leaf(path), "name" | "branch" | "repo_path")
}

fn toggle_row(
    k: &Kit,
    compact: bool,
    key: &str,
    label: &str,
    help: Option<&str>,
    on: bool,
    flip: fn(&mut State, &mut Tx),
) -> El {
    let mut col_body = col().gap(2.0).grow(1.0).child(
        text(label.to_string())
            .font(k.ty.small_medium)
            .color(k.c.text_base),
    );
    if let Some(help) = help {
        col_body = col_body.child(
            text(help.to_string())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    row()
        .w_full()
        .min_h(if compact { 44.0 } else { 34.0 })
        .gap(12.0)
        .center_y()
        .child(col_body)
        .child(
            rune_kit::Switch::new(on)
                .label(label)
                .hide_label()
                .on_toggle_tx(flip)
                .build(k)
                .min_h(if compact { 44.0 } else { 0.0 })
                .key(key),
        )
}

// ---------------------------------------------------------------------------------------------
// Projects

fn projects_list(s: &State, k: &Kit, compact: bool) -> El {
    let e = &s.editor;
    if !e.loaded {
        return widgets::empty_state(
            k,
            "Reading the config…",
            "The daemon has not answered yet. Check the connection in the banner.",
        );
    }
    let mut c = col()
        .w_full()
        .px(if compact { 14.0 } else { 22.0 })
        .py(if compact { 14.0 } else { 18.0 })
        .gap(12.0);
    if let Some(field) = &e.field {
        let message = e.error.clone().unwrap_or_default();
        c = c.child(widgets::notice(
            k,
            theme::status(k).failure,
            &format!("{field}: {message}"),
        ));
    }
    c = c.child(widgets::section(
        k,
        "Projects",
        Some(
            "Repositories the daemon watches, in poll order. Each has its own CI and deploy steps.",
        ),
    ));
    if e.count("project") == 0 {
        c = c.child(widgets::notice(
            k,
            k.c.text_faint,
            "No projects yet. Add one below.",
        ));
    }
    for i in 0..e.count("project") {
        c = c.child(project_card(s, k, i, compact));
    }
    c = c.child(
        row().w_full().gap(8.0).child(
            Button::new("Add project")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Regular)
                .on_press_tx(|s: &mut State, tx: &mut Tx| s.add_project(tx))
                .build(k)
                .key("projects-add"),
        ),
    );
    c
}

fn project_card(s: &State, k: &Kit, i: usize, compact: bool) -> El {
    let e = &s.editor;
    let name = e.s(&format!("project[{i}].name"));
    let branch = e.s(&format!("project[{i}].branch"));
    let ci = e.count(&format!("project[{i}].ci"));
    let deploy = e.count(&format!("project[{i}].deploy"));
    let invalid = e.invalid(&format!("project[{i}].name"));
    let info = col()
        .gap(2.0)
        .grow(1.0)
        .child(
            text(if name.is_empty() {
                "(unnamed)".to_string()
            } else {
                name.clone()
            })
            .font(k.ty.small_medium)
            .color(if name.is_empty() {
                k.c.text_faint
            } else {
                k.c.text_base
            })
            .wrap(),
        )
        .child(
            text(format!("{branch} · {ci} CI · {deploy} deploy"))
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
        );
    let edit_btn = Button::new("Edit")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_project_form(Some(i), tx))
        .build(k)
        .key(format!("project-edit-{i}"));

    if compact {
        return col()
            .key(format!("project-card-{i}"))
            .w_full()
            .padding(12.0)
            .gap(10.0)
            .radius(k.radius.md)
            .fill(k.c.layer_02)
            .border(
                1.0,
                if invalid {
                    theme::status(k).failure
                } else {
                    k.c.border_muted
                },
            )
            .child(info)
            .child(
                row()
                    .w_full()
                    .gap(6.0)
                    .center_y()
                    .child(up_down(k, i, true))
                    .child(el().grow(1.0))
                    .child(edit_btn),
            );
    }

    row()
        .key(format!("project-card-{i}"))
        .w_full()
        .padding(12.0)
        .gap(10.0)
        .center_y()
        .radius(k.radius.md)
        .fill(k.c.layer_02)
        .border(
            1.0,
            if invalid {
                theme::status(k).failure
            } else {
                k.c.border_muted
            },
        )
        .child(info)
        .child(up_down(k, i, false))
        .child(edit_btn)
}

fn up_down(k: &Kit, i: usize, compact: bool) -> El {
    row()
        .gap(if compact { 6.0 } else { 2.0 })
        .child(widgets::compact_icon(
            rune_kit::IconButton::new(&rune_kit::glyphs::CHEVRON_UP)
                .label("Move up")
                .control_size(ControlSize::Small)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.move_project(i, false, tx))
                .build(k)
                .key(format!("project-up-{i}")),
            compact,
        ))
        .child(widgets::compact_icon(
            rune_kit::IconButton::new(&rune_kit::glyphs::CHEVRON_DOWN)
                .label("Move down")
                .control_size(ControlSize::Small)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.move_project(i, true, tx))
                .build(k)
                .key(format!("project-down-{i}")),
            compact,
        ))
}

/// One project's form: identity, then its CI and deploy steps.
fn projects_detail(s: &State, k: &Kit, cx: &mut Cx<State>, compact: bool) -> El {
    let _ = cx;
    let e = &s.editor;
    if !e.loaded {
        return widgets::empty_state(
            k,
            "Reading the config…",
            "The daemon has not answered yet. Check the connection in the banner.",
        );
    }
    let Some(i) = e.project else {
        return projects_list(s, k, compact);
    };
    let base = format!("project[{i}]");
    let mut c = col()
        .key("project-form")
        .w_full()
        .px(if compact { 14.0 } else { 22.0 })
        .py(if compact { 14.0 } else { 18.0 })
        .gap(20.0);

    if let Some(field) = &e.field {
        let message = e.error.clone().unwrap_or_default();
        c = c.child(widgets::notice(
            k,
            theme::status(k).failure,
            &format!("{field}: {message}"),
        ));
    }

    let all_btn = Button::new("All projects")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press_tx(|s: &mut State, tx: &mut Tx| s.open_project_form(None, tx))
        .build(k)
        .key("project-all");
    let dup_btn = Button::new("Duplicate")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.duplicate_project(i, tx))
        .build(k)
        .key("project-duplicate");
    let del_btn = Button::new("Delete")
        .variant(ButtonVariant::Danger)
        .control_size(ControlSize::Small)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
            s.editor.confirm_delete = Some(i);
            tx.instant();
        })
        .build(k)
        .key("project-delete");

    if compact {
        c = c.child(
            row()
                .w_full()
                .gap(8.0)
                .flex_wrap()
                .center_y()
                .child(all_btn)
                .child(dup_btn)
                .child(del_btn),
        );
    } else {
        c = c.child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(all_btn)
                .child(el().grow(1.0))
                .child(dup_btn)
                .child(del_btn),
        );
    }

    if e.confirm_delete == Some(i) {
        let label = e.s(&format!("{base}.name"));
        let msg = text(format!(
            "Delete {label} and its steps? The config is only written on Save."
        ))
        .font(k.ty.small)
        .color(theme::status(k).failure);
        let cancel_btn = Button::new("Cancel")
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press_tx(|s: &mut State, tx: &mut Tx| {
                s.editor.confirm_delete = None;
                tx.instant();
            })
            .build(k)
            .key("project-delete-cancel");
        let confirm_btn = Button::new("Delete")
            .variant(ButtonVariant::Danger)
            .control_size(ControlSize::Small)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.delete_project(i, tx))
            .build(k)
            .key("project-delete-confirm");
        c = c.child(if compact {
            col()
                .w_full()
                .padding(12.0)
                .gap(10.0)
                .radius(k.radius.md)
                .fill(theme::status(k).tint(theme::status(k).failure))
                .child(msg.wrap())
                .child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .flex_wrap()
                        .child(cancel_btn.grow(1.0))
                        .child(confirm_btn.grow(1.0)),
                )
        } else {
            row()
                .w_full()
                .padding(12.0)
                .gap(10.0)
                .center_y()
                .radius(k.radius.md)
                .fill(theme::status(k).tint(theme::status(k).failure))
                .child(msg.grow(1.0))
                .child(cancel_btn)
                .child(confirm_btn)
        });
    }

    c = c.child(widgets::section(k, "Repository", None));
    c = c.child(widgets::pair_for(
        compact,
        text_field(
            s,
            k,
            &format!("{base}.name"),
            "Name",
            Some("Unique; used in URLs and on disk."),
        ),
        text_field(
            s,
            k,
            &format!("{base}.branch"),
            "Branch",
            Some("The branch a run is built from."),
        ),
    ));
    c = c.child(text_field(
        s,
        k,
        &format!("{base}.repo_path"),
        "Repository path",
        Some("The local clone the daemon fetches into and builds from."),
    ));
    c = c.child(widgets::pair_for(
        compact,
        text_field(
            s,
            k,
            &format!("{base}.remote"),
            "Remote",
            Some("Usually `origin`."),
        ),
        text_field(
            s,
            k,
            &format!("{base}.github_repo"),
            "GitHub repository",
            Some("`owner/name`, for commit links and checks."),
        ),
    ));
    c = c.child(widgets::pair_for(
        compact,
        select_field(
            s,
            k,
            compact,
            &format!("{base}.checkout"),
            "Checkout",
            &[
                ("none", "None (build in the repo)"),
                ("worktree", "Worktree per run"),
                ("in-place", "Reset in place"),
            ],
        ),
        u64_field(
            s,
            k,
            &format!("{base}.poll_interval"),
            "Poll interval (seconds)",
            Some("Empty uses the global interval."),
            false,
        ),
    ));
    c = c.child(secret_field(
        s,
        k,
        &format!("{base}.discord_webhook_url"),
        "Discord webhook",
        Some("Overrides the global webhook for this project."),
    ));
    c = c.child(table_field(
        s,
        k,
        &format!("{base}.env"),
        "Environment",
        Some("`KEY=value`, one per line, for every step in this project."),
    ));

    c = c.child(widgets::divider(k));
    c = c.child(steps::phase(s, k, i, false, compact));
    c = c.child(widgets::divider(k));
    c = c.child(steps::phase(s, k, i, true, compact));
    c
}

/// A choice row bound to a config path.
fn select_field(
    s: &State,
    k: &Kit,
    compact: bool,
    path: &str,
    label: &str,
    options: &[(&str, &str)],
) -> El {
    let current = crate::paths::get_str(&s.editor.draft, path);
    let pairs: Vec<(String, String)> = options
        .iter()
        .map(|(v, l)| (v.to_string(), l.to_string()))
        .collect();
    let owner = path.to_string();
    let pick = widgets::choice_row_for(
        k,
        compact,
        &pairs,
        Some(current.as_str()),
        None,
        move |s: &mut State, picked: &str, tx: &mut Tx| {
            s.editor.set_str(&owner, picked);
            tx.instant();
        },
    )
    .key(format!("select-{path}"));
    widgets::labelled(k, label, None, pick).key(format!("field-{path}"))
}

/// A `KEY=value`-per-line table bound to a config path.
pub(crate) fn table_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    let seeded = crate::paths::get_table_text(&s.editor.draft, path);
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let owner = path.to_string();
    let field = path.to_string();
    let input = TextArea::new(&edit)
        .label(label)
        .width(None)
        .rows(3)
        .auto_grow(crate::theme::area_min_rows(k), 8)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_table_text(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            crate::paths::set_table(&mut s.editor.draft, &owner, &text);
            let now_seeded = crate::paths::get_table_text(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
            tx.instant();
        })
        .build(k);
    let mut f = Field::new().label(label).control(input.key("control"));
    if let Some(help) = help {
        f = f.prefix(help);
    }
    f.build(k).key(format!("field-{path}"))
}

// ---------------------------------------------------------------------------------------------
// Connection (this window, not the daemon)

fn connection_page(s: &State, k: &Kit, compact: bool) -> El {
    let mut c = col()
        .w_full()
        .px(if compact { 14.0 } else { 22.0 })
        .py(if compact { 14.0 } else { 18.0 })
        .gap(18.0);
    c = c.child(widgets::section(k, "Connection", Some("Where this window looks for the daemon. Not written to config.toml; in a browser the token is remembered for this site.")));
    c = c.child(widgets::pair_for(
        compact,
        text_input_bound(
            k,
            "conn-server",
            "Server URL",
            &s.ui_server,
            |s: &mut State| &mut s.ui_server,
        ),
        text_input_bound(k, "conn-token", "Token", &s.ui_token, |s: &mut State| {
            &mut s.ui_token
        }),
    ));
    c = c.child(
        row()
            .w_full()
            .gap(8.0)
            .flex_wrap()
            .child(
                Button::new("Connect")
                    .variant(ButtonVariant::Submit)
                    .control_size(ControlSize::Regular)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| s.reconnect(tx))
                    .build(k)
                    .key("conn-connect"),
            )
            .children(
                (cfg!(target_arch = "wasm32")).then_some(
                    Button::new("Use this page's origin")
                        .variant(ButtonVariant::Outline)
                        .control_size(ControlSize::Regular)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| {
                            s.ui_server = TextEdit::new(crate::state::default_server());
                            tx.instant();
                        })
                        .build(k)
                        .key("conn-origin"),
                ),
            ),
    );
    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(k, "Status", None));
    c = c.child(widgets::kv_for(
        k,
        compact,
        "Server",
        widgets::value_for(k, compact, &s.server_display()),
    ));
    c = c.child(widgets::kv_for(
        k,
        compact,
        "State",
        match &s.link {
            crate::state::Link::Connecting => widgets::pill(k, "Connecting", k.c.text_muted),
            crate::state::Link::Ok => widgets::pill(k, "Connected", theme::status(k).success),
            crate::state::Link::Down { failures, .. } => {
                widgets::pill(k, format!("Offline ({failures})"), theme::status(k).failure)
            }
            crate::state::Link::Unauthorized => {
                widgets::pill(k, "Unauthorized", theme::status(k).error)
            }
        },
    ));
    c = c.child(widgets::kv_for(
        k,
        compact,
        "Daemon version",
        widgets::value_for(
            k,
            compact,
            if s.version.is_empty() {
                "—"
            } else {
                &s.version
            },
        ),
    ));
    c = c.child(widgets::kv_for(
        k,
        compact,
        "Launch flags",
        widgets::value_for(
            k,
            compact,
            "--server URL --token T, or LUNACY_SERVER / LUNACY_TOKEN",
        ),
    ));
    c
}

fn text_input_bound(
    k: &Kit,
    key: &str,
    label: &str,
    edit: &TextEdit,
    access: fn(&mut State) -> &mut TextEdit,
) -> El {
    let input = TextInput::new(edit)
        .label(label)
        .placeholder(label)
        .width(None)
        .on_edit(access)
        .build(k);
    Field::new()
        .label(label)
        .control(input.key("control"))
        .build(k)
        .key(key)
}
