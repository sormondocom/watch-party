mod core;
mod disc;
mod player;
mod stream;
mod sync;
mod tui;

use std::path::PathBuf;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let source_path: Option<PathBuf> = std::env::args().nth(1).map(PathBuf::from);
    let config = core::config::HostConfig::default();

    let mut app = tui::App::new(source_path, config);
    tui::run(&mut app).await
}
