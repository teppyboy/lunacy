//! The settings shell: a sidebar of pages, the page body, and the save bar.
//!
//! The daemon's config is edited as the JSON it already is (`Editor::draft`), so unknown keys
//! survive a save and a rejected field is addressed by the daemon's own path.

use rune::{col, el, row, text, Cx, El, ScrollSpec, Spring, TextEdit, Transition, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Field, Kit, TextArea, TextInput};

use crate::state::{SettingsPage, State};
use crate::theme;
use crate::views::{deploy, diagnostics, steps};
use crate::widgets;

pub fn view(
    s: &State,
    k: &Kit,
    page: SettingsPage,
    cx: &mut Cx<State>,
    hair: f32,
    compact: bool,
    layout_w: f32,
) -> El {
    let page_key = match page {
        SettingsPage::Projects if s.editor.project.is_some() => {
            format!("settings-page-Projects-{}", s.editor.project.unwrap_or(0))
        }
        _ => format!("settings-page-{}", page.title()),
    };
    let inner_page = match page {
        SettingsPage::Deploy => deploy::view(s, k, cx, compact),
        SettingsPage::Diagnostics => diagnostics::view(s, k, cx, compact),
        SettingsPage::Connection => connection_page(s, k, compact),
        SettingsPage::Appearance => appearance_page(s, k, cx, compact),
        SettingsPage::Projects if s.editor.project.is_some() => projects_detail(s, k, cx, compact),
        SettingsPage::Projects => projects_list(s, k, compact),
        other => config_page(s, k, other, cx, compact),
    };
    let page_shell = col()
        .key(page_key)
        .w_full()
        .transition(Transition::asymmetric(
            Transition::opacity().combined(Transition::offset(0.0, 5.0)),
            Transition::none(),
        ))
        .transition_motion(rune::tok::motion::SNAP, Spring::NONE)
        .child(inner_page);

    let content = widgets::scroll_bar(
        col()
            .key("settings-body")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .h_full()
            .scroll(ScrollSpec::default())
            .child(page_shell),
        k,
    );

    let body = if compact {
        let mut strip = row().gap(6.0).center_y();
        for p in SettingsPage::CONFIG.into_iter().chain(SettingsPage::WINDOW) {
            strip = strip.child(page_chip(k, s, p));
        }
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(widgets::scroll_bar_x(
                row()
                    .key("settings-nav-strip")
                    .w_full()
                    .px(14.0)
                    .pt(8.0)
                    .pb(18.0)
                    .fill(k.c.layer_01)
                    .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
                    .scroll_x()
                    .scroll_edge_fade(14.0)
                    .child(strip),
                k,
            ))
            .child(content)
    } else {
        let mut pages_list = col().w_full().gap(1.0).py(10.0);
        pages_list = pages_list.child(
            row()
                .w_full()
                .px(16.0)
                .pb(8.0)
                .child(widgets::caption(k, "CONFIG")),
        );
        for p in SettingsPage::CONFIG {
            pages_list = pages_list.child(page_row(k, s, p));
        }
        pages_list = pages_list.child(
            row()
                .w_full()
                .px(16.0)
                .pt(14.0)
                .pb(8.0)
                .child(widgets::caption(k, "THIS WINDOW")),
        );
        for p in SettingsPage::WINDOW {
            pages_list = pages_list.child(page_row(k, s, p));
        }
        let pages_w = if layout_w < 960.0 { 172.0 } else { 212.0 };
        let pages = col()
            .key("settings-pages")
            .w(pages_w)
            .shrink()
            .h_full()
            .min_h(0.0)
            .pr(theme::SECTION_BORDER)
            .fill(k.c.layer_01)
            .border_sides([0.0, theme::SECTION_BORDER, 0.0, 0.0], k.c.border_strong)
            .child(widgets::scroll_bar(
                col()
                    .key("settings-pages-scroll")
                    .w_full()
                    .grow(1.0)
                    .min_h(0.0)
                    .scroll(ScrollSpec::default())
                    .child(pages_list),
                k,
            ));
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
        .press_scale(0.985)
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
        .pl(16.0)
        .pr(18.0)
        .center_y()
        .fill(if selected {
            k.c.layer_03
        } else {
            rune::Color::TRANSPARENT
        })
        .hover(k.c.hover)
        .press_scale(0.985)
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
    let local = page.is_local();
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

    let _ = hair;
    if compact {
        let mut bar = col()
            .key("settings-bar")
            .w_full()
            .px(14.0)
            .py(12.0)
            .gap(10.0)
            .fill(k.c.layer_01)
            .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
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
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, theme::SECTION_BORDER, 0.0], k.c.border_strong)
        .child(
            col().gap(1.0).grow(1.0).min_w(0.0).child(title_row).child(
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
        .pl(if compact { 14.0 } else { 22.0 })
        .pr(if compact { 18.0 } else { 22.0 })
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
        SettingsPage::General => general(s, k, compact),
        SettingsPage::Server => server(s, k),
        SettingsPage::Github => github(s, k, compact),
        SettingsPage::Discord => discord(s, k, compact),
        SettingsPage::Act => act(s, k, compact),
        _ => col(),
    });
    body
}

fn general(s: &State, k: &Kit, compact: bool) -> El {
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
    c = c.child(u64_field(
        s,
        k,
        "general.step_timeout",
        "Default step timeout (seconds)",
        Some("Used when a project or step leaves timeout empty. Defaults to 3600s."),
        false,
    ));
    c = c.child(argv_field(
        s,
        k,
        "general.shell",
        "Default shell",
        Some("Command used to run shell steps, e.g. `bash -lc`. Empty uses the platform default."),
    ));
    let trigger_mode = e.s("general.trigger_mode");
    let trigger_idx = match trigger_mode.as_str() {
        "webhook" => Some(1),
        "poll" => Some(2),
        _ => Some(0),
    };
    c = c.child(
        widgets::labelled(
            k,
            "Trigger mode",
            Some("Webhook mode triggers runs immediately from GitHub webhooks without polling git remotes."),
            widgets::segmented(
                k,
                compact,
                &["Both", "Webhook", "Poll"],
                trigger_idx,
                |s: &mut State, picked: usize, _tx: &mut Tx| {
                    let val = match picked {
                        1 => "webhook",
                        2 => "poll",
                        _ => "both",
                    };
                    s.editor.set_str("general.trigger_mode", val);
                },
            )
            .key("general-trigger-mode"),
        )
        .key("field-general.trigger_mode"),
    );
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
            |s: &mut State, picked: usize, _tx: &mut Tx| {
                s.editor
                    .set_str("github.mode", if picked == 0 { "checks" } else { "status" });
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
    c = c.child(widgets::section(
        k,
        "Webhooks",
        Some("Receive GitHub webhook events at `/api/github/webhook` to trigger runs immediately."),
    ));
    c = c.child(secret_field(
        s,
        k,
        "github.webhook_secret",
        "Webhook secret",
        Some("Shared HMAC-SHA256 secret for repository webhooks or GitHub Apps."),
    ));

    let public_url = e.s("server.public_url");
    let sys_wh = s.system.as_ref().and_then(|sys| sys.webhook.as_ref());
    let payload_url = if !public_url.trim().is_empty() {
        format!(
            "{}/api/github/webhook",
            public_url.trim().trim_end_matches('/')
        )
    } else if let Some(ep) = sys_wh
        .map(|w| w.endpoint.as_str())
        .filter(|ep| !ep.is_empty())
    {
        ep.to_string()
    } else {
        "/api/github/webhook".to_string()
    };
    let missing_public_url = public_url.trim().is_empty()
        && sys_wh.is_none_or(|w| {
            !w.endpoint.starts_with("http://") && !w.endpoint.starts_with("https://")
        });

    let mut wh_card = col()
        .key("github-webhook-info")
        .w_full()
        .padding(14.0)
        .gap(6.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base)
        .child(widgets::caption(k, "WEBHOOK ENDPOINT"))
        .child(widgets::kv_for(
            k,
            compact,
            "Payload URL",
            widgets::value_for(k, compact, &payload_url),
        ))
        .child(widgets::divider(k))
        .child(widgets::kv_for(
            k,
            compact,
            "Content type",
            widgets::value_for(k, compact, "application/json"),
        ))
        .child(widgets::divider(k))
        .child(widgets::kv_for(
            k,
            compact,
            "Events",
            widgets::value_for(k, compact, "push, pull_request, check_run"),
        ));

    if let Some(wh) = sys_wh {
        let mut status_text = format!(
            "{} {}",
            wh.deliveries,
            if wh.deliveries == 1 {
                "delivery"
            } else {
                "deliveries"
            }
        );
        if let Some(ev) = wh.last_event.as_deref().filter(|v| !v.is_empty()) {
            status_text.push_str(&format!(" · last: {ev}"));
            if let Some(repo) = wh.last_repo.as_deref().filter(|v| !v.is_empty()) {
                status_text.push_str(&format!(" ({repo})"));
            }
        }
        let status_summary = text(status_text)
            .font(k.ty.small)
            .color(k.c.text_muted)
            .min_w(0.0);
        wh_card = wh_card.child(widgets::divider(k)).child(widgets::kv_for(
            k,
            compact,
            "Live status",
            row()
                .gap(8.0)
                .center_y()
                .flex_wrap()
                .child(if wh.configured {
                    widgets::pill(k, "Configured", theme::status(k).success)
                } else {
                    widgets::pill(k, "Not configured", k.c.text_faint)
                })
                .child(if compact {
                    status_summary.wrap_anywhere().shrink()
                } else {
                    status_summary.wrap().shrink()
                }),
        ));
    }

    if missing_public_url {
        wh_card = wh_card.child(
            text("Set Server → Public URL so GitHub can deliver webhooks to a full URL.")
                .font(k.ty.caption())
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    c = c.child(wh_card);

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
        |s: &mut State, _tx: &mut Tx| {
            let on = crate::paths::get_bool(&s.editor.draft, "discord.notify_on_start");
            crate::paths::set_bool(&mut s.editor.draft, "discord.notify_on_start", !on);
        },
    ));
    let notify_on = e.s("discord.notify_on");
    let notify_idx = match notify_on.as_str() {
        "failures" => Some(1),
        _ => Some(0),
    };
    c = c.child(
        widgets::labelled(
            k,
            "Notify on",
            Some("Choose whether to post on every finished run or only failed/errored runs."),
            widgets::segmented(
                k,
                compact,
                &["All runs", "Failures only"],
                notify_idx,
                |s: &mut State, picked: usize, _tx: &mut Tx| {
                    let val = if picked == 1 { "failures" } else { "all" };
                    s.editor.set_str("discord.notify_on", val);
                },
            )
            .key("discord-notify-on"),
        )
        .key("field-discord.notify_on"),
    );
    c = c.child(text_field_placeholder(
        s,
        k,
        "discord.username",
        "Bot username",
        "lunacy",
        Some("Optional display name for the Discord webhook bot (max 80 characters)."),
    ));
    c = c.child(text_field_placeholder(
        s,
        k,
        "discord.mention_on_failure",
        "Mention on failure",
        "<@&role_id> or <@user_id>",
        Some("Prepended to failure and error notifications (e.g. `<@&role_id>` or `<@user_id>`)."),
    ));
    let dirty = e.dirty();
    let can_test = !s.testing_discord && !dirty && e.loaded;
    c = c.child(
        row().w_full().gap(8.0).flex_wrap().child(
            Button::new(if s.testing_discord {
                "Sending test…"
            } else {
                "Send test message"
            })
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .disabled(!can_test)
            .help(if dirty {
                "Save your config changes before sending a test message"
            } else {
                "Send a test message to the configured global Discord webhook"
            })
            .on_press_tx(|s: &mut State, tx: &mut Tx| s.test_discord(None, tx))
            .build(k)
            .key("discord-test-global"),
        ),
    );
    c
}

fn act(s: &State, k: &Kit, compact: bool) -> El {
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

    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(
        k,
        "Build cache",
        Some("Persist language and package caches (Cargo, npm, Flutter, Go, pip, Gradle, etc.) across act runs."),
    ));
    c = c.child(toggle_row(
        k,
        compact,
        "field-act.cache",
        "Aggressive build cache",
        Some("Mount persistent per-project caches and enable act's action cache server. Defaults on when unset."),
        crate::paths::get_bool_or(&e.draft, "act.cache", true),
        |s: &mut State, _tx: &mut Tx| {
            let on = crate::paths::get_bool_or(&s.editor.draft, "act.cache", true);
            crate::paths::set_bool(&mut s.editor.draft, "act.cache", !on);
        },
    ));
    c = c.child(text_field(
        s,
        k,
        "act.cache_dir",
        "Cache directory",
        Some("Where persistent build caches are stored. Empty uses `cache` inside the state dir."),
    ));
    c = c.child(
        row().w_full().gap(8.0).flex_wrap().child(
            Button::new(if s.clearing_cache {
                "Clearing caches…"
            } else {
                "Clear all build caches"
            })
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .disabled(s.clearing_cache)
            .on_press_tx(|s: &mut State, tx: &mut Tx| s.clear_cache(None, tx))
            .build(k)
            .key("act-clear-cache"),
        ),
    );
    c
}

/// A text field bound to a config path, with inline error and help.
pub(crate) fn text_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    text_field_placeholder(s, k, path, label, label, help)
}

/// A text field bound to a config path with a custom placeholder, inline error and help.
pub(crate) fn text_field_placeholder(
    s: &State,
    k: &Kit,
    path: &str,
    label: &str,
    placeholder: &str,
    help: Option<&str>,
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
        .placeholder(placeholder)
        .width(None)
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_str(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, _tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            let optional = is_optional(&owner);
            s.editor.set_str_mode(&owner, &text, optional);
            prune_empty_parents(&mut s.editor.draft, &owner);
            let now_seeded = crate::paths::get_str(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
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
        .on_change(move |s: &mut State, _tx: &mut Tx| {
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
            prune_empty_parents(&mut s.editor.draft, &owner);
            let now_seeded = crate::paths::get_str(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
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
        .on_change(move |s: &mut State, _tx: &mut Tx| {
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
        .scroll_bar_styled(widgets::scroll_bar_style(k))
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_list_text(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, _tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            crate::paths::set_list(&mut s.editor.draft, &owner, &text);
            let now_seeded = crate::paths::get_list_text(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
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
        .on_change(move |s: &mut State, _tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            s.editor.set_str(&owner, &text);
            prune_empty_parents(&mut s.editor.draft, &owner);
            let now_seeded = crate::paths::get_str(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
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
        .scroll_bar_styled(widgets::scroll_bar_style(k))
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_str(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, _tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            s.editor.set_str(&owner, &text);
            prune_empty_parents(&mut s.editor.draft, &owner);
            let now_seeded = crate::paths::get_str(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
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

fn prune_empty_parents(draft: &mut serde_json::Value, path: &str) {
    if path.starts_with("github.app.") {
        crate::paths::prune_empty_object(draft, "github.app");
    }
    if path.starts_with("github.") {
        crate::paths::prune_empty_object(draft, "github");
    }
    if path.starts_with("discord.") {
        crate::paths::prune_empty_object(draft, "discord");
    }
    if let Some((parent, _leaf)) = path.rsplit_once('.') {
        if parent.ends_with(".contexts") {
            crate::paths::prune_empty_object(draft, parent);
        }
    }
}

/// A tri-state `Option<bool>` segmented picker: Inherit (`null`), On (`true`), Off (`false`).
fn tri_bool_segmented(
    s: &State,
    k: &Kit,
    compact: bool,
    path: &str,
    seg_key: &str,
    label: &str,
    help: Option<&str>,
) -> El {
    let selected = match crate::paths::get(&s.editor.draft, path) {
        Some(serde_json::Value::Bool(true)) => Some(1),
        Some(serde_json::Value::Bool(false)) => Some(2),
        _ => Some(0),
    };
    let owner = path.to_string();
    widgets::labelled(
        k,
        label,
        help,
        widgets::segmented(
            k,
            compact,
            &["Inherit", "On", "Off"],
            selected,
            move |s: &mut State, picked: usize, _tx: &mut Tx| match picked {
                1 => s.editor.set(&owner, serde_json::Value::Bool(true)),
                2 => s.editor.set(&owner, serde_json::Value::Bool(false)),
                _ => s.editor.set(&owner, serde_json::Value::Null),
            },
        )
        .key(seg_key),
    )
    .key(format!("field-{path}"))
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
    flip: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    let mut col_body = col().gap(2.0).min_w(0.0).grow(1.0).child(
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
        .pl(if compact { 14.0 } else { 22.0 })
        .pr(if compact { 18.0 } else { 22.0 })
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
    let enabled = crate::paths::get_bool_or(&e.draft, &format!("project[{i}].enabled"), true);
    let ci = e.count(&format!("project[{i}].ci"));
    let deploy = e.count(&format!("project[{i}].deploy"));
    let invalid = e.invalid(&format!("project[{i}].name"));
    let info = col()
        .gap(2.0)
        .grow(1.0)
        .min_w(0.0)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .flex_wrap()
                .center_y()
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
                    .wrap_anywhere(),
                )
                .children((!enabled).then(|| widgets::pill(k, "Paused", k.c.text_faint))),
        )
        .child(
            text(format!("{branch} · {ci} CI · {deploy} deploy"))
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap_anywhere(),
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
                    k.c.border_base
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
                k.c.border_base
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
        .pl(if compact { 14.0 } else { 22.0 })
        .pr(if compact { 18.0 } else { 22.0 })
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
        .on_press_tx(move |s: &mut State, _tx: &mut Tx| {
            s.editor.confirm_delete = Some(i);
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
            .on_press_tx(|s: &mut State, _tx: &mut Tx| {
                s.editor.confirm_delete = None;
            })
            .build(k)
            .key("project-delete-cancel");
        let confirm_btn = Button::new("Delete")
            .variant(ButtonVariant::Danger)
            .control_size(ControlSize::Small)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.delete_project(i, tx))
            .build(k)
            .key("project-delete-confirm");
        let delete_banner = if compact {
            col()
                .key("project-delete-banner")
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
                .key("project-delete-banner")
                .w_full()
                .padding(12.0)
                .gap(10.0)
                .center_y()
                .radius(k.radius.md)
                .fill(theme::status(k).tint(theme::status(k).failure))
                .child(msg.wrap().min_w(0.0).grow(1.0))
                .child(cancel_btn)
                .child(confirm_btn)
        }
        .transition(Transition::asymmetric(
            Transition::opacity().combined(Transition::offset(0.0, -4.0)),
            Transition::none(),
        ))
        .transition_motion(rune::tok::motion::SNAP, Spring::NONE);
        c = c.child(delete_banner);
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
    c = c.child(widgets::pair_for(
        compact,
        u64_field(
            s,
            k,
            &format!("{base}.step_timeout"),
            "Default step timeout (seconds)",
            Some("Overrides the global step timeout for this project's steps."),
            false,
        ),
        u64_field(
            s,
            k,
            &format!("{base}.timeout"),
            "Total pipeline timeout (seconds)",
            Some(
                "Maximum total duration across checkout, CI, and deploy. Empty means no overall cap.",
            ),
            false,
        ),
    ));
    let enabled_path = format!("{base}.enabled");
    c = c.child(toggle_row(
        k,
        compact,
        &format!("field-{enabled_path}"),
        "Enabled (automatic polling & webhooks)",
        Some("Turn off to pause automatic polling and webhook runs for this repo. Manual triggers stay available."),
        crate::paths::get_bool_or(&e.draft, &enabled_path, true),
        move |s: &mut State, _tx: &mut Tx| {
            let on = crate::paths::get_bool_or(&s.editor.draft, &enabled_path, true);
            crate::paths::set_bool(&mut s.editor.draft, &enabled_path, !on);
        },
    ));
    c = c.child(table_field(
        s,
        k,
        &format!("{base}.env"),
        "Environment",
        Some("`KEY=value`, one per line, for every step in this project."),
    ));

    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(
        k,
        "Pull requests, deploys & cache",
        Some("Control trigger mode, which branches and tags deploy, whether PRs run CI, and per-project build caching."),
    ));
    let trigger_mode_path = format!("{base}.trigger_mode");
    let trigger_mode_val = e.s(&trigger_mode_path);
    let trigger_mode_idx = match trigger_mode_val.as_str() {
        "both" => Some(1),
        "webhook" => Some(2),
        "poll" => Some(3),
        _ => Some(0),
    };
    let trigger_mode_owner = trigger_mode_path.clone();
    c = c.child(
        widgets::labelled(
            k,
            "Trigger mode",
            None,
            widgets::segmented(
                k,
                compact,
                &["Inherit", "Both", "Webhook", "Poll"],
                trigger_mode_idx,
                move |s: &mut State, picked: usize, _tx: &mut Tx| match picked {
                    1 => s.editor.set_str(&trigger_mode_owner, "both"),
                    2 => s.editor.set_str(&trigger_mode_owner, "webhook"),
                    3 => s.editor.set_str(&trigger_mode_owner, "poll"),
                    _ => s.editor.set(&trigger_mode_owner, serde_json::Value::Null),
                },
            )
            .key(format!("project-trigger-mode-{i}")),
        )
        .key(format!("field-{trigger_mode_path}")),
    );
    let watch_prs_path = format!("{base}.watch_prs");
    c = c.child(toggle_row(
        k,
        compact,
        &format!("field-{watch_prs_path}"),
        "Build pull requests (CI only)",
        Some("Automatically run CI steps on open GitHub pull requests. Deploy steps are always skipped for PRs."),
        crate::paths::get_bool(&e.draft, &watch_prs_path),
        move |s: &mut State, _tx: &mut Tx| {
            let on = crate::paths::get_bool(&s.editor.draft, &watch_prs_path);
            crate::paths::set_bool(&mut s.editor.draft, &watch_prs_path, !on);
        },
    ));
    c = c.child(list_field(
        s,
        k,
        &format!("{base}.deploy_refs"),
        "Deploy branches / tags",
        Some("One branch or tag pattern per line (e.g. master, release/*, tags/v*). Empty deploys only the watched branch; PRs never deploy."),
    ));
    let watch_tags_path = format!("{base}.watch_tags");
    c = c.child(toggle_row(
        k,
        compact,
        &format!("field-{watch_tags_path}"),
        "Watch matching tags",
        Some("Automatically build and deploy new git tags that match `deploy_refs`."),
        crate::paths::get_bool(&e.draft, &watch_tags_path),
        move |s: &mut State, _tx: &mut Tx| {
            let on = crate::paths::get_bool(&s.editor.draft, &watch_tags_path);
            crate::paths::set_bool(&mut s.editor.draft, &watch_tags_path, !on);
        },
    ));
    let cache_path = format!("{base}.cache");
    let cache_default = crate::paths::get_bool_or(&e.draft, "act.cache", true);
    c = c.child(toggle_row(
        k,
        compact,
        &format!("field-{cache_path}"),
        "Aggressive build cache",
        Some("Persist compiler and package manager caches across runs for this project."),
        crate::paths::get_bool_or(&e.draft, &cache_path, cache_default),
        move |s: &mut State, _tx: &mut Tx| {
            let def = crate::paths::get_bool_or(&s.editor.draft, "act.cache", true);
            let on = crate::paths::get_bool_or(&s.editor.draft, &cache_path, def);
            crate::paths::set_bool(&mut s.editor.draft, &cache_path, !on);
        },
    ));
    let project_name = e.s(&format!("{base}.name"));
    let test_project_name = project_name.clone();
    let unsaved_project = e.new_project || project_name.trim().is_empty();
    let can_clear = !s.clearing_cache && !unsaved_project;
    c = c.child(
        row().w_full().gap(8.0).flex_wrap().child(
            Button::new(if s.clearing_cache {
                "Clearing cache…"
            } else {
                "Clear project cache"
            })
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .disabled(!can_clear)
            .help(if unsaved_project {
                "Save this project before clearing its cache"
            } else {
                "Remove cached build artifacts for this project"
            })
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.clear_cache(Some(&project_name), tx))
            .build(k)
            .key("project-clear-cache"),
        ),
    );

    c = c.child(widgets::divider(k));
    c = c.child(steps::phase(s, k, i, false, compact));
    c = c.child(widgets::divider(k));
    c = c.child(steps::phase(s, k, i, true, compact));

    // Discord notifications (per-repo)
    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(
        k,
        "Discord notifications",
        Some("Override global Discord settings or mute notifications for this repository."),
    ));
    let discord_enabled_path = format!("{base}.discord_enabled");
    c = c.child(tri_bool_segmented(
        s,
        k,
        compact,
        &discord_enabled_path,
        &format!("project-discord-enabled-{i}"),
        "Discord notifications",
        Some(
            "Off disables Discord messages for this repo even when a global webhook is configured.",
        ),
    ));
    c = c.child(secret_field(
        s,
        k,
        &format!("{base}.discord_webhook_url"),
        "Discord webhook URL",
        Some("Overrides the global webhook URL for this repo."),
    ));
    c = c.child(tri_bool_segmented(
        s,
        k,
        compact,
        &format!("{base}.notify_on_start"),
        &format!("project-notify-on-start-{i}"),
        "Notify on start",
        Some("Post when a run begins, or inherit the global Discord setting."),
    ));
    let discord_notify_on_path = format!("{base}.discord_notify_on");
    let discord_notify_on_val = e.s(&discord_notify_on_path);
    let discord_notify_on_idx = match discord_notify_on_val.as_str() {
        "all" => Some(1),
        "failures" => Some(2),
        _ => Some(0),
    };
    let discord_notify_on_owner = discord_notify_on_path.clone();
    c = c.child(
        widgets::labelled(
            k,
            "Notify on",
            Some("Override whether this repo posts on every finished run or only failures (start notifications are controlled above)."),
            widgets::segmented(
                k,
                compact,
                &["Inherit", "All runs", "Failures only"],
                discord_notify_on_idx,
                move |s: &mut State, picked: usize, _tx: &mut Tx| match picked {
                    1 => s.editor.set_str(&discord_notify_on_owner, "all"),
                    2 => s.editor.set_str(&discord_notify_on_owner, "failures"),
                    _ => s
                        .editor
                        .set(&discord_notify_on_owner, serde_json::Value::Null),
                },
            )
            .key(format!("project-discord-notify-on-{i}")),
        )
        .key(format!("field-{discord_notify_on_path}")),
    );
    c = c.child(widgets::pair_for(
        compact,
        text_field_placeholder(
            s,
            k,
            &format!("{base}.discord_username"),
            "Bot username",
            "lunacy",
            Some("Overrides the global Discord bot display name (max 80 characters)."),
        ),
        text_field_placeholder(
            s,
            k,
            &format!("{base}.discord_mention_on_failure"),
            "Mention on failure",
            "<@&role_id> or <@user_id>",
            Some("Prepended to failure notifications for this repo. Empty inherits global."),
        ),
    ));
    let discord_disabled = matches!(
        crate::paths::get(&e.draft, &discord_enabled_path),
        Some(serde_json::Value::Bool(false))
    );
    let dirty = e.dirty();
    let can_test_discord = !s.testing_discord && !unsaved_project && !dirty && !discord_disabled;
    c = c.child(
        row().w_full().gap(8.0).flex_wrap().child(
            Button::new(if s.testing_discord {
                "Sending test…"
            } else {
                "Test Discord webhook"
            })
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .disabled(!can_test_discord)
            .help(if unsaved_project {
                "Save this project before testing its Discord webhook"
            } else if dirty {
                "Save your config changes before sending a test message"
            } else if discord_disabled {
                "Discord notifications are turned off for this repo"
            } else {
                "Send a test message using this repo's effective Discord webhook"
            })
            .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                s.test_discord(Some(test_project_name.clone()), tx)
            })
            .build(k)
            .key("project-discord-test"),
        ),
    );

    // GitHub reporting (per-repo)
    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(
        k,
        "GitHub reporting",
        Some("Control whether runs report back to GitHub and customize check or status context names."),
    ));
    c = c.child(tri_bool_segmented(
        s,
        k,
        compact,
        &format!("{base}.github_enabled"),
        &format!("project-github-enabled-{i}"),
        "GitHub reporting",
        Some("Off skips GitHub check runs and commit statuses for this repo."),
    ));
    c = c.child(widgets::pair_for(
        compact,
        text_field_placeholder(
            s,
            k,
            &format!("{base}.contexts.ci"),
            "CI check / status context",
            "lunacy/ci",
            Some("Check run or commit status name for CI. Empty uses `lunacy/ci`."),
        ),
        text_field_placeholder(
            s,
            k,
            &format!("{base}.contexts.deploy"),
            "Deploy check / status context",
            "lunacy/deploy",
            Some("Check run or commit status name for deploy. Empty uses `lunacy/deploy`."),
        ),
    ));

    // Runner overrides (per-repo)
    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(
        k,
        "Runner overrides",
        Some("Override the default shell or act runner settings for steps in this repository."),
    ));
    c = c.child(argv_field(
        s,
        k,
        &format!("{base}.shell"),
        "Default shell",
        Some("Per-repo shell override, e.g. `bash -lc`. Empty inherits the global shell."),
    ));
    c = c.child(table_field(
        s,
        k,
        &format!("{base}.platforms"),
        "act platforms",
        Some("`runs-on=image`, one per line. Overrides global `[act.platforms]` for this repo."),
    ));
    c = c.child(list_field(
        s,
        k,
        &format!("{base}.act_args"),
        "Extra act arguments",
        Some("Extra arguments passed to `act` for this repo, one per line."),
    ));
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
        move |s: &mut State, picked: &str, _tx: &mut Tx| {
            s.editor.set_str(&owner, picked);
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
        .scroll_bar_styled(widgets::scroll_bar_style(k))
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_table_text(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, _tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            crate::paths::set_table(&mut s.editor.draft, &owner, &text);
            let now_seeded = crate::paths::get_table_text(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
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
        .pl(if compact { 14.0 } else { 22.0 })
        .pr(if compact { 18.0 } else { 22.0 })
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
                        .on_press_tx(|s: &mut State, _tx: &mut Tx| {
                            s.ui_server = TextEdit::new(crate::state::default_server());
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

// ---------------------------------------------------------------------------------------------
// Appearance (this window, not the daemon)

fn format_zoom_value(z: f32) -> String {
    if (z * 100.0 - (z * 10.0).round() * 10.0).abs() < 0.1 {
        format!("{z:.1}")
    } else {
        format!("{z:.2}")
    }
}

fn appearance_page(s: &State, k: &Kit, cx: &Cx<State>, compact: bool) -> El {
    let mut c = col()
        .w_full()
        .pl(if compact { 14.0 } else { 22.0 })
        .pr(if compact { 18.0 } else { 22.0 })
        .py(if compact { 14.0 } else { 18.0 })
        .gap(18.0);

    c = c.child(widgets::section(
        k,
        "Appearance",
        Some("Window theme and interface scale. Saved locally for this window or browser."),
    ));

    let theme_options: Vec<(String, String)> = theme::ThemeMode::ALL
        .iter()
        .map(|m| (m.as_str().to_string(), m.label().to_string()))
        .collect();
    c = c.child(widgets::labelled(
        k,
        "Theme",
        Some(match s.theme_mode {
            theme::ThemeMode::System => {
                if cx.system_dark() {
                    "Following system appearance (currently Dark)."
                } else {
                    "Following system appearance (currently Light)."
                }
            }
            theme::ThemeMode::Light => "Always light surfaces.",
            theme::ThemeMode::Dark => "Always standard dark surfaces.",
            theme::ThemeMode::Oled => {
                "Pure #000000 black surfaces defined by crisp neutral borders."
            }
        }),
        widgets::choice_row_for(
            k,
            compact,
            &theme_options,
            Some(s.theme_mode.as_str()),
            None,
            |s: &mut State, picked: &str, tx: &mut Tx| {
                if let Some(mode) = theme::ThemeMode::parse(picked) {
                    s.set_theme_mode(mode, tx);
                }
            },
        )
        .key("appearance-theme"),
    ));

    c = c.child(widgets::divider(k));

    let zoom_options: Vec<(String, String)> = crate::state::ZOOM_STEPS
        .iter()
        .map(|&z| {
            let v = format_zoom_value(z);
            (v.clone(), format!("{v}×"))
        })
        .collect();
    let cur = s.current_zoom();
    let active_zoom = crate::state::ZOOM_STEPS
        .iter()
        .copied()
        .find(|&z| (z - cur).abs() < 0.02)
        .map(format_zoom_value);
    c = c.child(widgets::labelled(
        k,
        "Zoom",
        Some("Interface scale across all screens. Shortcuts: Ctrl/Cmd+=, Ctrl/Cmd+-, Ctrl/Cmd+0."),
        widgets::choice_row_for(
            k,
            compact,
            &zoom_options,
            active_zoom.as_deref(),
            None,
            |s: &mut State, picked: &str, tx: &mut Tx| {
                if let Ok(z) = picked.parse::<f32>() {
                    s.set_user_zoom(Some(z), tx);
                }
            },
        )
        .key("appearance-zoom"),
    ));

    let zoom_status = match s.user_zoom {
        Some(z) => format!("Custom scale: {}×", format_zoom_value(z)),
        None => format!(
            "Automatic default ({:.1}× on {})",
            cur,
            if compact { "compact" } else { "desktop" }
        ),
    };
    c = c.child(
        row()
            .w_full()
            .gap(10.0)
            .center_y()
            .flex_wrap()
            .child(
                Button::new("Reset to default")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Regular)
                    .disabled(s.user_zoom.is_none())
                    .on_press_tx(|s: &mut State, tx: &mut Tx| s.set_user_zoom(None, tx))
                    .build(k)
                    .key("appearance-zoom-reset"),
            )
            .child(
                text(zoom_status)
                    .font(k.ty.small)
                    .color(k.c.text_muted)
                    .wrap(),
            ),
    );

    c = c.child(widgets::divider(k));
    c = c.child(toggle_row(
        k,
        compact,
        "appearance-log-wrap",
        "Word wrap logs",
        Some("Wrap long log lines to fit the pane instead of scrolling horizontally."),
        s.log_wrap,
        |s: &mut State, tx: &mut Tx| s.toggle_log_wrap(tx),
    ));

    #[cfg(not(target_arch = "wasm32"))]
    {
        c = c.child(widgets::divider(k));
        c = c.child(desktop_updates_section(s, k, compact));
    }

    c
}

#[cfg(not(target_arch = "wasm32"))]
fn desktop_updates_section(s: &State, k: &Kit, compact: bool) -> El {
    use crate::updater::{self, UpdateStatus};

    let build_str = format!(
        "v{} · {} · {}",
        updater::BUILD_VERSION,
        updater::BUILD_SHORT_SHA,
        updater::BUILD_TARGET
    );

    let mut c = col()
        .key("desktop-updates")
        .w_full()
        .gap(14.0)
        .child(widgets::section(
            k,
            "Desktop Updates",
            Some("Check GitHub prereleases and update lunacy-ui in place."),
        ))
        .child(widgets::kv_for(
            k,
            compact,
            "Current build",
            widgets::value_for(k, compact, &build_str),
        ))
        .child(toggle_row(
            k,
            compact,
            "appearance-auto-update",
            "Check for updates on startup",
            Some("Automatically check for a newer desktop build shortly after launch."),
            s.auto_update_check,
            |s: &mut State, tx: &mut Tx| s.toggle_auto_update_check(tx),
        ));

    let busy = s.update_status.is_busy();
    let check_btn = Button::new(if matches!(s.update_status, UpdateStatus::Checking) {
        "Checking…"
    } else {
        "Check now"
    })
    .variant(ButtonVariant::Outline)
    .control_size(ControlSize::Small)
    .disabled(busy)
    .on_press_tx(|s: &mut State, tx: &mut Tx| s.check_for_updates(tx))
    .build(k)
    .key("update-check-now");

    let mut actions = row()
        .w_full()
        .gap(8.0)
        .flex_wrap()
        .center_y()
        .child(check_btn);

    match &s.update_status {
        UpdateStatus::Available(info) => {
            if info.asset_url.is_some() {
                actions = actions.child(
                    Button::new("Install update")
                        .variant(ButtonVariant::Submit)
                        .control_size(ControlSize::Small)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| s.install_update(tx))
                        .build(k)
                        .key("update-install-now"),
                );
            }
            if !info.html_url.is_empty() {
                let url = info.html_url.clone();
                actions = actions.child(
                    Button::new("View on GitHub")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                            let _ = tx.open_url(&url);
                        })
                        .build(k)
                        .key("update-view-github"),
                );
            }
        }
        UpdateStatus::ReadyToRestart(info) => {
            actions = actions.child(
                Button::new("Restart to apply")
                    .variant(ButtonVariant::Submit)
                    .control_size(ControlSize::Small)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| s.restart_for_update(tx))
                    .build(k)
                    .key("update-restart-now"),
            );
            if !info.html_url.is_empty() {
                let url = info.html_url.clone();
                actions = actions.child(
                    Button::new("View on GitHub")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                            let _ = tx.open_url(&url);
                        })
                        .build(k)
                        .key("update-view-github"),
                );
            }
        }
        _ => {}
    }

    c = c.child(actions);

    let status_el = match &s.update_status {
        UpdateStatus::Idle => text("Not checked yet in this session.")
            .font(k.ty.small)
            .color(k.c.text_faint)
            .wrap(),
        UpdateStatus::Checking => text("Checking GitHub prerelease…")
            .font(k.ty.small)
            .color(k.c.text_muted)
            .wrap(),
        UpdateStatus::UpToDate { checked_sha } => text(format!(
            "Up to date with the latest prerelease ({checked_sha})."
        ))
        .font(k.ty.small)
        .color(theme::status(k).success)
        .wrap(),
        UpdateStatus::Available(info) => {
            let suffix = if info.asset_url.is_none() {
                " (no automatic download for this target — open on GitHub)"
            } else {
                ""
            };
            let published = if info.published_at.is_empty() {
                String::new()
            } else {
                format!(" · {}", info.published_at)
            };
            widgets::notice(
                k,
                theme::status(k).running,
                &format!(
                    "Update available: {} ({}){published}{suffix}",
                    info.title, info.short_sha
                ),
            )
        }
        UpdateStatus::Installing { info, stage } => widgets::notice(
            k,
            theme::status(k).running,
            &format!("Installing {}: {stage}", info.short_sha),
        ),
        UpdateStatus::ReadyToRestart(info) => widgets::notice(
            k,
            theme::status(k).success,
            &format!(
                "Update {} installed. Click \"Restart to apply\" to launch the new version.",
                info.short_sha
            ),
        ),
        UpdateStatus::Error(err) => widgets::notice(k, theme::status(k).failure, err),
    };
    c = c.child(status_el);
    c
}
