//! Glyphst Reader: GPU vector pages, tabs and split panes, a library home
//! and shader effects for transitions and backgrounds.
//!
//! glyphst-reader [documents…] [--background ID] [--transition ID|none] [--zoom Z]
//!     [--split right|down] [--rotate 90|180|270] [--spread] [--overlay ID]
//!     [--wallpaper PATH] [--opacity O] [--page N] [--find TEXT] [--select-page N] [--select-pages A-B] [--sidebar TAB] [--font-size PT]
//!     [--translate-fake] [--translate-document] [--translate-selection] [--ai-fake] [--ask TEXT] [--agent claude-code|codex|pi|acp:NAME]
//!     [--palette QUERY] [--settings appearance|reading|effects|translation|ocr|ai|shortcuts]
//!     [--bench | --screenshot PATH | --screenshot-turn PATH | --screenshot-switch PATH]
//!
//! Mouse: Ctrl+wheel or pinch zooms at the pointer; wheel and drag scroll,
//! dragging on text selects it (Alt+drag a column), double and triple
//! clicks select a word and a line, right-click for highlights and notes.
//! Keys: Ctrl+O open, Ctrl+W close, Ctrl+Tab switch tabs; + / - / = (or 0) zoom
//! and fit, Page Up / Page Down / Space turn pages, Home / End, P presents,
//! Ctrl+F find (F3 next), Ctrl+C copy, Ctrl+A select the page, Ctrl+D
//! bookmark, Ctrl+T translate the selection, Ctrl+L assistant, Alt+Left /
//! Alt+Right back and forward, Ctrl+\ split right, F6 next pane, F9
//! sidebar, F12 statistics. Dragging a tab onto the pages splits them.

mod activity_view;
mod ai;
mod ai_bridge;
mod ai_changes;
mod ai_editor;
mod ai_mentions;
mod ai_menu;
mod ai_preview;
mod ai_tools;
mod annotations;
mod app;
mod book_effects;
mod commands;
mod covers;
mod desktop_theme;
mod doc_text;
mod document;
mod document_tab;
mod effects_page;
mod fake_providers;
mod find;
mod gamepad;
mod generation;
mod gpu;
mod harness;
mod home;
mod keymap_page;
mod keys;
mod launcher;
mod library_view;
mod overview;
mod pages;
mod palette;
mod panes;
mod pickers;
mod prefs;
mod present;
mod quick_ask;
mod reading_clock;
mod reading_days;
mod region;
mod settings_providers;
mod settings_window;
mod shell;
mod sidebar;
mod text_overlay;
mod transition;
mod translation;
mod user_settings;
mod wallpaper;
mod window_blur;

use std::path::PathBuf;

use glyphst_ui::{fonts, icons, theme};

use anyhow::{Context as _, Result, bail};
use app::{Launch, ReaderApp};
use harness::{Bench, Harness, Screenshot, ShotKind};

const USAGE: &str = "usage: glyphst-reader [documents…] [--background ID] [--transition ID|none] [--theme system|light|dark] [--zoom Z] [--window WxH] [--split right|down] [--rotate DEGREES] [--spread] [--overlay ID] [--wallpaper PATH] [--opacity O] [--colours preset|background|custom] \
                     [--translate-fake] [--translate-document] [--translate-selection] [--ai-fake] [--ask TEXT] [--agent claude-code|codex|pi|acp:NAME] \
                     [--assistant-demo STATE] [--gallery] [--palette QUERY] [--settings appearance|reading|effects|translation|ocr|ai|shortcuts[:GROUP]] [--command ID]… [--select-region PAGE:X0,Y0,X1,Y1] \
                     [--page N] [--find TEXT] [--select-page N] [--select-pages A-B] [--sidebar outline|pages|bookmarks|notes|search] [--font-size PT] \
                     [--bench | --screenshot PATH | --screenshot-turn PATH | --screenshot-switch PATH | --screenshot-generating PATH | --screenshot-revealing PATH | --screenshot-present-tools PATH | --screenshot-slide PATH]";

fn parse_args() -> Result<Launch> {
    let mut launch = Launch {
        files: Vec::new(),
        harness: None,
        background: None,
        transition: None,
        theme: None,
        zoom: None,
        window: None,
        split: None,
        overlay: None,
        wallpaper: None,
        opacity: None,
        colours: None,
        arrangement: None,
        find: None,
        page: None,
        select_pages: None,
        sidebar: None,
        font_size: None,
        translate_fake: false,
        translate_document: false,
        translate_selection: false,
        ai_fake: false,
        ask: None,
        assistant_demo: None,
        agent: None,
        palette: None,
        settings: None,
        settings_group: None,
        shortcuts: false,
        gallery: false,
        commands: Vec::new(),
        select_region: None,
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
            "--screenshot-generating" => launch.harness = shot(ShotKind::Generating, value()?),
            "--screenshot-revealing" => launch.harness = shot(ShotKind::Revealing, value()?),
            "--screenshot-present-tools" => launch.harness = shot(ShotKind::PresentTools, value()?),
            "--screenshot-slide" => launch.harness = shot(ShotKind::SidebarSlide, value()?),
            "--background" => launch.background = Some(value()?),
            "--overlay" => launch.overlay = Some(value()?),
            "--wallpaper" => launch.wallpaper = Some(value()?),
            "--opacity" => {
                launch.opacity = Some(value()?.parse().context("--opacity takes a number")?);
            }
            "--rotate" => {
                let rotation = match value()?.as_str() {
                    "0" => 0,
                    "90" => 1,
                    "180" => 2,
                    "270" => 3,
                    other => bail!("--rotate takes 0, 90, 180 or 270, not {other}"),
                };
                launch.arrangement.get_or_insert_default().rotation = rotation;
            }
            "--spread" => launch.arrangement.get_or_insert_default().spread = true,
            "--split" => {
                launch.split = match value()?.as_str() {
                    "right" => Some(craft_ui::docking::Zone::Right),
                    "down" => Some(craft_ui::docking::Zone::Bottom),
                    other => bail!("--split takes right or down, not {other}"),
                };
            }
            "--theme" => {
                let name = value()?;
                launch.theme = Some(
                    theme::ThemePreference::ALL
                        .into_iter()
                        .find(|theme| theme.label().eq_ignore_ascii_case(&name))
                        .with_context(|| {
                            format!("--theme takes system, light or dark, not {name}")
                        })?,
                );
            }
            "--transition" => launch.transition = Some(value()?),
            "--zoom" => launch.zoom = Some(value()?.parse().context("--zoom takes a number")?),
            "--window" => {
                let size = value()?;
                let (width, height) = size
                    .split_once('x')
                    .context("--window takes WIDTHxHEIGHT")?;
                launch.window = Some([
                    width.parse().context("--window width")?,
                    height.parse().context("--window height")?,
                ]);
            }
            "--find" => launch.find = Some(value()?),
            "--translate-fake" => launch.translate_fake = true,
            "--translate-document" => launch.translate_document = true,
            "--translate-selection" => launch.translate_selection = true,
            "--ai-fake" => launch.ai_fake = true,
            "--ask" => launch.ask = Some(value()?),
            "--assistant-demo" => {
                let state = value()?;
                if !crate::ai::DEMO_STATES.contains(&state.as_str()) {
                    bail!(
                        "unknown assistant state {state} ({})",
                        crate::ai::DEMO_STATES.join(", ")
                    );
                }
                launch.assistant_demo = Some(state);
            }
            "--agent" => {
                let id = value()?;
                if !["claude-code", "codex", "pi"].contains(&id.as_str()) && !id.starts_with("acp:")
                {
                    bail!("unknown agent {id} (claude-code, codex, pi or acp:NAME)");
                }
                launch.agent = Some(id);
            }
            "--palette" => launch.palette = Some(value()?),
            "--gallery" => launch.gallery = true,
            "--select-region" => {
                let usage = "--select-region takes PAGE:X0,Y0,X1,Y1 (page points)";
                let value = value()?;
                let (page, rect) = value.split_once(':').context(usage)?;
                let numbers: Vec<f32> = rect
                    .split(',')
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .context(usage)?;
                let [x0, y0, x1, y1] = numbers[..] else {
                    bail!(usage);
                };
                let page: usize = page.parse().context(usage)?;
                launch.select_region = Some((page.max(1), [x0, y0, x1, y1]));
            }
            "--command" => {
                let id = value()?;
                launch.commands.push(
                    commands::Command::from_id(&id)
                        .with_context(|| format!("unknown command {id}"))?,
                );
            }
            "--colours" => {
                let name = value()?;
                launch.colours = Some(match name.as_str() {
                    "preset" => prefs::Colours::Preset,
                    "background" => prefs::Colours::Background,
                    "custom" => prefs::Colours::Custom,
                    _ => bail!("--colours takes preset, background or custom, not {name}"),
                });
            }
            "--settings" => {
                let value = value()?;
                let (name, group) = match value.split_once(':') {
                    Some((name, group)) => (name.to_string(), Some(group.to_string())),
                    None => (value, None),
                };
                launch.settings_group = group;
                // The shortcuts have their own tab.
                if name.eq_ignore_ascii_case("shortcuts") {
                    launch.shortcuts = true;
                } else {
                    launch.settings =
                        Some(settings_window::Section::parse(&name).with_context(|| {
                            format!("unknown settings section {name}\n{USAGE}")
                        })?);
                }
            }
            "--page" => {
                let page: usize = value()?.parse().context("--page takes a page number")?;
                launch.page = Some(page.max(1));
            }
            "--select-page" => {
                let page: usize = value()?
                    .parse()
                    .context("--select-page takes a page number")?;
                launch.select_pages = Some((page.max(1), page.max(1)));
            }
            "--select-pages" => {
                let pages = value()?;
                let (first, last) = pages
                    .split_once('-')
                    .context("--select-pages takes FIRST-LAST")?;
                let first: usize = first.parse().context("--select-pages takes FIRST-LAST")?;
                let last: usize = last.parse().context("--select-pages takes FIRST-LAST")?;
                launch.select_pages = Some((first.max(1), last.max(first).max(1)));
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

/// egui's wgpu setup with smaller GPU memory blocks. wgpu's default
/// (`MemoryHints::Performance`) suballocates from 128-256 MiB blocks, and the
/// first submission using a new block waits while the driver prepares all
/// of it: 16-23 ms on Intel Arc, a dropped frame whenever pages outgrow the
/// blocks. With 8-32 MiB blocks a new one costs a few milliseconds.
fn wgpu_configuration() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    let default_descriptor = setup.device_descriptor.clone();
    setup.device_descriptor = std::sync::Arc::new(move |adapter| wgpu::DeviceDescriptor {
        memory_hints: wgpu::MemoryHints::Manual {
            suballocated_device_memory_block_size: (8 << 20)..(32 << 20),
        },
        ..default_descriptor(adapter)
    });
    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// Prints the file dialogs' warnings and errors, which otherwise vanish:
/// rfd says only through `log` why a dialog didn't open (no libdbus, no
/// portal, no zenity).
struct DialogLog;

impl log::Log for DialogLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn && metadata.target().starts_with("rfd")
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{}: {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

fn main() -> Result<()> {
    if log::set_logger(&DialogLog).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
    // Translation typesets pages with Typst in a worker process that runs
    // this executable.
    if let Some(exit_code) = glyphst_pdf_translation_service::run_typst_worker_if_requested() {
        std::process::exit(exit_code);
    }
    // Agents that start MCP servers only as commands run this executable
    // as a relay to the running Reader's server.
    if std::env::args().nth(1).as_deref() == Some("--mcp-stdio") {
        return glyphst_agent_host::mcp_stdio::run();
    }
    let launch = parse_args()?;
    // Scripted runs keep no preferences, so they cannot change the user's.
    let persistence_path = launch
        .harness
        .is_none()
        .then(|| {
            // The path of the egui Reader's preferences before it became the
            // only Reader, so they carry over.
            dirs::config_dir().map(|dir| dir.join("glyphst").join("reader-egui").join("app.ron"))
        })
        .flatten();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Glyphst Reader")
        .with_app_id("glyphst-reader")
        .with_inner_size(launch.window.unwrap_or([1440.0, 920.0]))
        .with_min_inner_size([720.0, 480.0]);
    // A fixed size, which tiling compositors keep.
    if let Some(size) = launch.window {
        viewport = viewport.with_min_inner_size(size).with_max_inner_size(size);
    }
    let native = eframe::NativeOptions {
        viewport: viewport
            .with_drag_and_drop(true)
            // Always able to show the desktop through, so transparency
            // turns on and off without a restart; opaque until it is on.
            .with_transparent(true),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_configuration(),
        persistence_path,
        ..Default::default()
    };
    eframe::run_native(
        "glyphst-reader",
        native,
        Box::new(move |cc| Ok(Box::new(ReaderApp::new(cc, launch)?))),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}
