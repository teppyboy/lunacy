//! `lunacy-client`: the daemon's HTTP API as seen by its clients (the Rune UI and `lunacy-cli`).
//!
//! - [`model`]: every JSON shape in `docs/api.md`, tolerant of unknown fields and enum values,
//!   so an older client keeps working against a newer daemon.
//! - [`config`]: the config document the UI edits (`GET/PUT /api/config`).
//! - [`api`]: one [`api::Call`] per endpoint and its parsed [`api::Answer`].
//! - [`net`]: the transport. Native builds block on `ureq`; `wasm32` builds `await` the
//!   browser's `fetch`.

pub mod api;
pub mod config;
pub mod model;
pub mod net;
