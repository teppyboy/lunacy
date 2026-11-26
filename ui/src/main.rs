//! `lunacy-ui`: native entry point. `cargo run -- --server http://127.0.0.1:7878 --token T`.

#[cfg(not(target_arch = "wasm32"))]
const APP_ICON_PNG: &[u8] = include_bytes!("../assets/lunacy-icon.png");

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    let _ = rune::set_app_icon(APP_ICON_PNG);
    lunacy_ui::build().run();
}
