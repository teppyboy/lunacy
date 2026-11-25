//! `lunacy-ui`: native entry point. `cargo run -- --server http://127.0.0.1:7878 --token T`.

fn main() {
    lunacy_ui::build().run();
}
