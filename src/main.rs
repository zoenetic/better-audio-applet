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

/// e.g. "0.1.0 (35c429b)", from build.rs.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_HASH"), ")");

fn main() -> cosmic::iced::Result {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("better-audio-applet {VERSION}");
            return Ok(());
        }
        Some("--watch-streams") => {
            watch_streams();
            return Ok(());
        }
        _ => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let _ = tracing_log::LogTracer::init();

    cosmic::applet::run::<app::Audio>(())
}

/// Print every sound stream's properties as it appears, and whether the applet
/// lists it, until interrupted. For finding out what a stray list entry is.
fn watch_streams() {
    println!("better-audio-applet {VERSION}: watching streams, Ctrl+C to stop");
    pulse::WATCH_STREAMS.store(true, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let _backend = backend::Handle::start(tx);
    futures::executor::block_on(futures::StreamExt::for_each(rx, |_| async {}));
}
