//! Glyphst Reader on egui: GPU vector pages, tabs, a library home and
//! shader effects for transitions and backgrounds.
//!
//! glyphst-reader-egui [documents…] [--background ID] [--transition ID|none] [--zoom Z]
//!     [--page N] [--find TEXT] [--select-page N] [--sidebar TAB] [--font-size PT]
//!     [--bench | --screenshot PATH | --screenshot-turn PATH | --screenshot-switch PATH]
//!
//! Mouse: Ctrl+wheel or pinch zooms at the pointer; wheel and drag scroll,
//! dragging on text selects it (Alt+drag a column), double and triple
//! clicks select a word and a line, right-click for highlights and notes.
//! Keys: Ctrl+O open, Ctrl+W close, Ctrl+Tab switch tabs; + / - / 0 zoom
//! and fit, Page Up / Page Down / Space turn pages, Home / End, P presents,
//! Ctrl+F find (F3 next), Ctrl+C copy, Ctrl+A select the page, Ctrl+D
//! bookmark, Alt+Left / Alt+Right back and forward, F9 sidebar, F12
//! statistics.

mod annotations;
mod app;
mod doc_text;
mod document;
mod document_tab;
mod find;
mod fonts;
mod gamepad;
mod gpu;
mod harness;
mod home;
mod pickers;
mod prefs;
mod sidebar;
mod text_overlay;
mod theme;
mod transition;

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use app::{Launch, ReaderApp};
use harness::{Bench, Harness, Screenshot, ShotKind};

const USAGE: &str = "usage: glyphst-reader-egui [documents…] [--background ID] [--transition ID|none] [--zoom Z] \
                     [--page N] [--find TEXT] [--select-page N] [--sidebar outline|pages|bookmarks|notes|search] [--font-size PT] \
                     [--bench | --screenshot PATH | --screenshot-turn PATH | --screenshot-switch PATH]";

fn parse_args() -> Result<Launch> {
    let mut launch = Launch {
        files: Vec::new(),
        harness: None,
        background: None,
        transition: None,
        zoom: None,
        find: None,
        page: None,
        select_page: None,
        sidebar: None,
        font_size: None,
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
            "--find" => launch.find = Some(value()?),
            "--page" => {
                let page: usize = value()?.parse().context("--page takes a page number")?;
                launch.page = Some(page.max(1));
            }
            "--select-page" => {
                let page: usize = value()?
                    .parse()
                    .context("--select-page takes a page number")?;
                launch.select_page = Some(page.max(1));
            }
            "--sidebar" => {
                let name = value()?;
                launch.sidebar = Some(
                    sidebar::SidebarTab::parse(&name)
                        .with_context(|| format!("unknown sidebar tab {name}\n{USAGE}"))?,
                );
            }
            "--font-size" => {
                launch.font_size = Some(value()?.parse().context("--font-size takes a number")?);
            }
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
