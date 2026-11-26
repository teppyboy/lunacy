//! `lunacy-ui`: native entry point. `cargo run -- --server http://127.0.0.1:7878 --token T`.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    configure_app_icon();
    lunacy_ui::build().run();
}

fn configure_app_icon() {
    #[cfg(target_os = "windows")]
    {
        rune::set_app_icon_resource(1);
    }

    #[cfg(target_os = "macos")]
    {
        let _ = rune::set_app_icon_choice(rune::AppIcon::Pair {
            light: include_bytes!("../../assets/icons/lunacy-macos-light-256.png").to_vec(),
            dark: include_bytes!("../../assets/icons/lunacy-macos-dark-256.png").to_vec(),
        });
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        rune::set_app_id("lunacy");
        let _ = rune::set_app_icon(include_bytes!("../../assets/icons/lunacy-256.png"));
    }
}
