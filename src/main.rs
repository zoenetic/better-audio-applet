// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod backend;
mod model;
mod mouse_area;
mod mpris_subscription;
mod pulse;
mod pw;

// Used by `mouse_area`, which comes from the stock COSMIC audio applet.
use cosmic::{Renderer, Theme};

pub const APP_ID: &str = "io.github.zoenetic.BetterAudioApplet";

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let _ = tracing_log::LogTracer::init();

    cosmic::applet::run::<app::Audio>(())
}
