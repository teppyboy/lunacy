//! `lunacy-ui`: a Rune client for the Lunacy local CI daemon.
//!
//! The UI only ever talks HTTP (`docs/api.md`), so it runs against a daemon on this machine, a
//! daemon on another one, or a mock. It builds natively (`cargo run`) and for the web
//! (`build-web.sh`), from one view tree.

pub mod api;
pub mod app;
pub mod config;
pub mod demo;
pub mod icons;
pub mod model;
pub mod net;
pub mod paths;
pub mod state;
pub mod theme;
pub mod updater;
pub mod views;
pub mod widgets;
pub mod ws;

pub use app::build;

/// The browser entry point. Without a `start` export the cdylib has nothing reachable, and the
/// optimiser strips the whole app down to an empty module.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    build().run();
}
