#![warn(clippy::pedantic)]
#![allow(
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use std::fs::{self, File};

use tracing::{Level, info};

use crate::app::App;

pub mod app;
pub mod event;
pub mod pano;
pub mod roadtrip;
pub mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Init tracing logs
    fs::create_dir_all("./logs")?;
    let log_file = File::options()
        .create(true)
        .append(true)
        .open("./logs/irtui.log")?;

    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .with_ansi(false)
        .with_writer(log_file)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    info!("Initializing terminal");
    let terminal = ratatui::init();
    info!("Lauching app");
    let result = App::default().run(terminal).await;
    info!("Exiting...");
    ratatui::restore();
    result
}
