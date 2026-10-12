//! Glyphst Reader on egui: GPU vector pages, tabs, a library home and
//! shader effects for transitions and backgrounds.
//!
//! glyphst-reader-egui [documents…] [--background ID] [--transition ID|none] [--zoom Z]
//!     [--bench | --screenshot PATH | --screenshot-turn PATH | --screenshot-switch PATH]
//!
//! Mouse: Ctrl+wheel or pinch zooms at the pointer; wheel and drag scroll.
//! Keys: Ctrl+O open, Ctrl+W close, Ctrl+Tab switch tabs; + / - / 0 zoom
//! and fit, Page Up / Page Down / Space turn pages, Home / End, P presents,
//! F9 outline, F12 statistics.

mod app;
mod document;
mod document_tab;
mod fonts;
mod gamepad;
mod gpu;
mod harness;
mod home;
mod pickers;
mod prefs;
mod theme;
mod transition;

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use app::{Launch, ReaderApp};
use harness::{Bench, Harness, Screenshot, ShotKind};

const USAGE: &str = "usage: glyphst-reader-egui [documents…] [--background ID] [--transition ID|none] [--zoom Z] \
                     [--bench | --screenshot PATH | --screenshot-turn PATH | --screenshot-switch PATH]";

fn parse_args() -> Result<Launch> {
    let mut launch = Launch {
        files: Vec::new(),
        harness: None,
        background: None,
        transition: None,
        zoom: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("{arg} needs a value\n{USAGE}"))
        };
        let shot = |kind, path: String| {
            Some(Harness::Screenshot(Screenshot::new(
                PathBuf::from(path),
                kind,
            )))
        };
        match arg.as_str() {
            "--bench" => launch.harness = Some(Harness::Bench(Bench::default())),
            "--screenshot" => launch.harness = shot(ShotKind::View, value()?),
            "--screenshot-turn" => launch.harness = shot(ShotKind::PageTurn, value()?),
            "--screenshot-switch" => launch.harness = shot(ShotKind::TabSwitch, value()?),
            "--background" => launch.background = Some(value()?),
            "--transition" => launch.transition = Some(value()?),
            "--zoom" => launch.zoom = Some(value()?.parse().context("--zoom takes a number")?),
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ if arg.starts_with("--") => bail!("unknown option {arg}\n{USAGE}"),
            _ => launch
                .files
                .push(std::path::absolute(&arg).unwrap_or_else(|_| PathBuf::from(&arg))),
        }
    }
    Ok(launch)
}

fn main() -> Result<()> {
    let launch = parse_args()?;
    // Scripted runs keep no preferences, so they cannot change the user's.
    let persistence_path = launch
        .harness
        .is_none()
        .then(|| {
            dirs::config_dir().map(|dir| dir.join("glyphst").join("reader-egui").join("app.ron"))
        })
        .flatten();
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Glyphst Reader")
            .with_app_id("glyphst-reader-egui")
            .with_inner_size([1440.0, 920.0])
            .with_min_inner_size([720.0, 480.0])
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Wgpu,
        persistence_path,
        ..Default::default()
    };
    eframe::run_native(
        "glyphst-reader-egui",
        native,
        Box::new(move |cc| Ok(Box::new(ReaderApp::new(cc, launch)?))),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}
