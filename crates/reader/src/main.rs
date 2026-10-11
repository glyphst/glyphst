use std::{path::PathBuf, time::Duration};

use anyhow::{Result, anyhow};
use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, AppContext, Bounds, Context, Entity, Global, Pixels, Task, Window, WindowBounds,
    WindowOptions, point, px, size,
};
use gpui_component::{Root, TitleBar};
use gpui_platform::application;
use tikv_jemalloc_sys as _;
use tikv_jemallocator::Jemalloc;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod fonts;
mod instance;

use glyphst_document_ui::document_view::RenderCache;
use glyphst_keymap::keybindings;
use glyphst_library::{
    DocumentPosition, LibraryStore, ReaderSessionSnapshot, ReaderWindowBounds, ReaderWindowSession,
    ReaderWindowState,
};
use glyphst_pdf_translation_service::run_typst_worker_if_requested;
use glyphst_settings::{RestoreOnStartup, SettingsStore, localize, sync_component_theme};
use glyphst_translation::TranslationService;
use glyphst_translation_ui::TranslationServices;
use glyphst_ui::{
    activity::{ActivityCenter, ActivityNoticeEntry, ActivityNoticeLevel},
    assets,
};
use glyphst_workspace::{
    NewTab, Workspace, WorkspaceCallbacks, WorkspaceWindowHandle, WorkspaceWindowRequest,
};

const APP_ID: &str = "glyphst-reader";
const READER_WINDOW_MIN_WIDTH: f32 = 900.0;
const READER_WINDOW_MIN_HEIGHT: f32 = 600.0;
const SESSION_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Clone)]
struct ReaderSessionPersistence(Entity<ReaderSessionPersistenceState>);

impl Global for ReaderSessionPersistence {}

struct ReaderSessionPersistenceState {
    suspended: bool,
    generation: u64,
    save_task: Option<Task<()>>,
}

impl ReaderSessionPersistenceState {
    fn observe_workspace(
        &mut self,
        workspace: &Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.observe(workspace, |this, _, cx| this.schedule_save(cx))
            .detach();
        cx.observe_window_bounds(window, |this, _, cx| this.schedule_save(cx))
            .detach();
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        self.suspended = false;
        self.schedule_save(cx);
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.suspended {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SESSION_SAVE_DEBOUNCE).await;
            _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.save_task = None;
                if let Err(error) = persist_reader_session(cx) {
                    eprintln!("failed to save reader session: {error:#}");
                }
            });
        }));
    }
}

fn main() {
    if let Some(exit_code) = run_typst_worker_if_requested() {
        std::process::exit(exit_code);
    }

    let startup = match instance::parse_args() {
        Ok(startup) => startup,
        Err(error) => {
            eprintln!("invalid Glyphst Reader arguments: {error:#}");
            std::process::exit(2);
        }
    };
    let request = startup.open;

    if let Err(error) = glyphst_core::paths::migrate_legacy_app_dirs() {
        eprintln!("failed to migrate Glyphst Reader data: {error:#}");
    }
    match instance::try_forward(&request) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => eprintln!("failed to forward open request: {error:#}"),
    }

    let (open_tx, mut open_rx) = mpsc::unbounded();
    if let Err(error) = instance::listen(open_tx) {
        eprintln!("failed to start IPC listener: {error:#}");
        return;
    }

    ztracing::init();

    application()
        .with_assets(assets::Assets::default())
        .run(move |cx: &mut App| {
            glyphst_ui::init(cx);
            keybindings::install_input_compatibility_bindings(cx);
            keybindings::capture_framework_bindings(cx);

            if let Err(error) = fonts::load_embedded_fonts(cx) {
                eprintln!("failed to load embedded fonts: {error:#}");
            }
            let settings_store = SettingsStore::load();
            if let Err(error) = settings_store.install_theme_catalog(cx) {
                eprintln!("failed to install theme catalog: {error:#}");
            }
            localize::select_language(settings_store.settings.language);
            cx.set_reduce_motion(settings_store.settings.behavior.reduce_motion);
            cx.set_global(settings_store);
            SettingsStore::initialize_provider_secrets(cx);
            glyphst_ai::chatgpt_subscription::init(cx);
            sync_component_theme(cx);
            cx.set_global(LibraryStore::load());
            cx.set_global(RenderCache::new());
            let session_persistence = cx.new(|_| ReaderSessionPersistenceState {
                suspended: true,
                generation: 0,
                save_task: None,
            });
            cx.set_global(ReaderSessionPersistence(session_persistence));
            match TranslationService::new() {
                Ok(service) => {
                    cx.set_global(TranslationServices::new(service.clone()));
                    cx.spawn(async move |cx| {
                        let result = cx
                            .background_spawn(async move { service.initialize_persistent_cache() })
                            .await;
                        if let Err(error) = result {
                            eprintln!(
                                "failed to initialize persistent translation cache: {error:#}"
                            );
                        }
                    })
                    .detach();
                }
                Err(error) => eprintln!("failed to initialize translation service: {error:#}"),
            }
            cx.on_app_quit(|cx| {
                if let Err(error) = persist_reader_session(cx) {
                    eprintln!("failed to save reader session: {error:#}");
                }
                if let Err(error) = cx.global_mut::<SettingsStore>().save() {
                    eprintln!("failed to save settings: {error:#}");
                }
                if let Err(error) = cx.global::<LibraryStore>().save() {
                    eprintln!("failed to save PDF library: {error:#}");
                }
                if let Err(error) = instance::remove_socket_file() {
                    eprintln!("failed to remove IPC socket: {error:#}");
                }
                async {}
            })
            .detach();
            cx.on_window_closed(|cx, _| {
                let persistence = cx.global::<ReaderSessionPersistence>().0.clone();
                persistence.update(cx, |persistence, cx| persistence.schedule_save(cx));
            })
            .detach();

            let (keymap_preset, keybinding_overrides) = {
                let store = cx.global::<SettingsStore>();
                (
                    store.settings.keymap_preset,
                    store.settings.keybinding_overrides.clone(),
                )
            };
            keybindings::apply(cx, keymap_preset, &keybinding_overrides);

            let restore_session = should_restore_reader_session(
                &request,
                cx.global::<SettingsStore>().settings.restore_on_startup,
            );
            let restored = if restore_session {
                cx.global::<LibraryStore>().reader_session().cloned()
            } else {
                None
            };
            let startup_result = if let Some(session) = restored
                && !session.windows.is_empty()
            {
                restore_reader_session(session, cx)
            } else {
                open_workspace_window(request.path.clone(), cx).map(|_| 0)
            };
            match startup_result {
                Ok(missing_documents) => {
                    if missing_documents > 0 {
                        show_missing_session_documents_notice(missing_documents, cx);
                    }
                }
                Err(error) => {
                    eprintln!("failed to open workspace window: {error:#}");
                    if let Err(fallback_error) = open_workspace_window(None, cx) {
                        eprintln!("failed to open fallback workspace window: {fallback_error:#}");
                        return;
                    }
                }
            }
            cx.global::<ReaderSessionPersistence>()
                .0
                .clone()
                .update(cx, |persistence, cx| persistence.resume(cx));

            cx.activate(true);

            cx.spawn(async move |cx| {
                while let Some(request) = open_rx.next().await {
                    cx.update(|cx| handle_open_request(request, cx));
                }
            })
            .detach();
        });
}

fn handle_open_request(request: instance::OpenRequest, cx: &mut App) {
    if request.new {
        if let Err(error) = open_workspace_window(request.path, cx) {
            eprintln!("failed to open workspace window: {error:#}");
        }
        cx.activate(true);
        return;
    }

    let path = request.path;
    let Some(workspace) = active_workspace_window(cx) else {
        if let Err(error) = open_workspace_window(path, cx) {
            eprintln!("failed to open workspace window: {error:#}");
        }
        cx.activate(true);
        return;
    };

    let update_path = path.clone();
    match workspace.update(cx, |workspace, window, cx| {
        if let Some(path) = update_path
            && let Err(error) = workspace.open_document_external(path, window, cx)
        {
            eprintln!("failed to open document: {error:#}");
        }
        window.activate_window();
    }) {
        Ok(()) => cx.activate(true),
        Err(error) => {
            eprintln!("failed to update workspace window: {error:#}");
            if let Err(error) = open_workspace_window(path, cx) {
                eprintln!("failed to open workspace window: {error:#}");
            }
            cx.activate(true);
        }
    }
}

fn should_restore_reader_session(
    request: &instance::OpenRequest,
    restore_on_startup: RestoreOnStartup,
) -> bool {
    !request.new && request.path.is_none() && restore_on_startup == RestoreOnStartup::LastSession
}

fn persist_reader_session(cx: &mut App) -> Result<()> {
    let mut windows = Vec::new();
    for window in cx.windows() {
        let Some(workspace) = WorkspaceWindowHandle::from_any(window, cx) else {
            continue;
        };
        let snapshot = workspace.update(cx, |workspace, window, cx| {
            if workspace.reader_session_restore_pending() {
                None
            } else {
                Some(workspace.reader_window_session(window, cx))
            }
        })?;
        let Some(snapshot) = snapshot else {
            // Do not replace a complete session with an intermediate tree while
            // its asynchronously opened document tabs are still resolving.
            return Ok(());
        };
        windows.push(snapshot);
    }
    if windows.is_empty() {
        // Closing the final reader window is also how the application exits;
        // retain its last non-empty snapshot for the next launch.
        return Ok(());
    }
    cx.global_mut::<LibraryStore>()
        .set_reader_session(ReaderSessionSnapshot { windows })
}

fn persist_reader_session_before_window_close(
    closing_workspace: &Entity<Workspace>,
    closing_window: &Window,
    cx: &mut App,
) -> Result<()> {
    let closing_window_id = closing_window.window_handle().window_id();
    let closing_snapshot = closing_workspace.update(cx, |workspace, cx| {
        if workspace.reader_session_restore_pending() {
            None
        } else {
            Some(workspace.reader_window_session(closing_window, cx))
        }
    });
    let Some(closing_snapshot) = closing_snapshot else {
        // Retain the last complete snapshot rather than persisting a partially
        // restored tree when the user closes a window during startup.
        return Ok(());
    };

    let mut remaining_windows = Vec::new();
    for window in cx.windows() {
        if window.window_id() == closing_window_id {
            continue;
        }
        let Some(workspace) = WorkspaceWindowHandle::from_any(window, cx) else {
            continue;
        };
        let snapshot = workspace.update(cx, |workspace, window, cx| {
            if workspace.reader_session_restore_pending() {
                None
            } else {
                Some(workspace.reader_window_session(window, cx))
            }
        })?;
        let Some(snapshot) = snapshot else {
            return Ok(());
        };
        remaining_windows.push(snapshot);
    }

    if remaining_windows.is_empty() {
        // Closing the final reader window exits the application on Linux and
        // Windows. Save it while GPUI still exposes its workspace and window.
        remaining_windows.push(closing_snapshot);
    }
    cx.global_mut::<LibraryStore>()
        .set_reader_session(ReaderSessionSnapshot {
            windows: remaining_windows,
        })
}

fn observe_reader_workspace(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    workspace.update(cx, |workspace, _| {
        workspace.set_window_close_handler(close_reader_workspace_window);
    });
    let persistence = cx.global::<ReaderSessionPersistence>().0.clone();
    persistence.update(cx, |persistence, cx| {
        persistence.observe_workspace(workspace, window, cx);
    });

    let workspace = workspace.downgrade();
    window.on_window_should_close(cx, move |window, cx| {
        if let Some(workspace) = workspace.upgrade()
            && let Err(error) = persist_reader_session_before_window_close(&workspace, window, cx)
        {
            eprintln!("failed to save reader session before closing window: {error:#}");
        }
        true
    });
}

fn close_reader_workspace_window(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    if let Err(error) = persist_reader_session_before_window_close(workspace, window, cx) {
        eprintln!("failed to save reader session before closing window: {error:#}");
    }
    window.remove_window();
}

fn restore_reader_session(session: ReaderSessionSnapshot, cx: &mut App) -> Result<usize> {
    let mut opened = 0_usize;
    let mut missing_documents = 0_usize;
    let mut last_error = None;
    for window_session in session.windows {
        match open_reader_session_window(window_session, cx) {
            Ok(missing) => {
                opened += 1;
                missing_documents += missing;
            }
            Err(error) => {
                eprintln!("failed to restore reader window: {error:#}");
                last_error = Some(error);
            }
        }
    }
    if opened == 0 {
        return Err(last_error.unwrap_or_else(|| anyhow!("reader session contains no windows")));
    }
    Ok(missing_documents)
}

fn show_missing_session_documents_notice(missing_documents: usize, cx: &mut App) {
    let notice = ActivityNoticeEntry::new(
        ActivityNoticeLevel::Warning,
        glyphst_workspace::fl!("reader-session-missing-title"),
        glyphst_workspace::fl!(
            "reader-session-missing-detail",
            count = missing_documents as i64
        ),
    );
    ActivityCenter::global(cx).update(cx, |center, cx| {
        center.add_notice(notice, cx);
    });
}

fn active_workspace_window(cx: &App) -> Option<WorkspaceWindowHandle> {
    cx.active_window()
        .and_then(|window| WorkspaceWindowHandle::from_any(window, cx))
        .or_else(|| {
            cx.window_stack().and_then(|windows| {
                windows
                    .into_iter()
                    .find_map(|window| WorkspaceWindowHandle::from_any(window, cx))
            })
        })
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|window| WorkspaceWindowHandle::from_any(window, cx))
        })
}

fn open_keymap_in_active_workspace(cx: &mut App) {
    let Some(workspace_window) = active_workspace_window(cx) else {
        return;
    };

    if let Err(error) = workspace_window.update(cx, |workspace, window, cx| {
        workspace.open_keymap_tab(window, cx);
        window.activate_window();
    }) {
        eprintln!("failed to open keymap editor: {error:#}");
    }
}

fn open_settings_window(cx: &mut App) {
    glyphst_settings_ui::open_settings_window(open_keymap_in_active_workspace, APP_ID, cx);
}

fn workspace_callbacks() -> WorkspaceCallbacks {
    WorkspaceCallbacks {
        open_workspace_window: open_detached_workspace_window,
        open_settings_window,
    }
}

fn open_workspace_window(path: Option<PathBuf>, cx: &mut App) -> Result<()> {
    open_workspace_window_at(path, None, cx)
}

fn open_workspace_window_at(
    path: Option<PathBuf>,
    bounds: Option<Bounds<Pixels>>,
    cx: &mut App,
) -> Result<()> {
    open_workspace_window_at_position(path, None, bounds, cx).map(|_| ())
}

fn open_detached_workspace_window(
    request: WorkspaceWindowRequest,
    cx: &mut App,
) -> Result<WorkspaceWindowHandle> {
    open_workspace_window_inner(None, None, Some(request.bounds), true, cx)
}

fn open_workspace_window_at_position(
    path: Option<PathBuf>,
    initial_position: Option<DocumentPosition>,
    bounds: Option<Bounds<Pixels>>,
    cx: &mut App,
) -> Result<WorkspaceWindowHandle> {
    open_workspace_window_inner(path, initial_position, bounds, false, cx)
}

fn open_workspace_window_inner(
    path: Option<PathBuf>,
    initial_position: Option<DocumentPosition>,
    bounds: Option<Bounds<Pixels>>,
    empty: bool,
    cx: &mut App,
) -> Result<WorkspaceWindowHandle> {
    let window_size = if path.is_some() {
        size(px(READER_WINDOW_MIN_WIDTH), px(1000.0))
    } else {
        size(px(NewTab::WINDOW_WIDTH), px(NewTab::WINDOW_HEIGHT))
    };
    let bounds = bounds.unwrap_or_else(|| Bounds::centered(None, window_size, cx));
    let window_settings = cx.global::<SettingsStore>().settings.window;
    let transparency_enabled = window_settings.transparency_enabled_by_default();

    let root = cx.open_window(
        WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(READER_WINDOW_MIN_WIDTH),
                px(READER_WINDOW_MIN_HEIGHT),
            )),
            window_background: window_settings.background_appearance(transparency_enabled),
            ..Default::default()
        },
        move |window, cx| {
            window.set_app_id(APP_ID);
            let workspace = cx.new(|cx| {
                if empty {
                    Workspace::new_empty(workspace_callbacks(), window, cx)
                } else {
                    Workspace::new(
                        path.clone(),
                        initial_position,
                        workspace_callbacks(),
                        window,
                        cx,
                    )
                }
            });
            observe_reader_workspace(&workspace, window, cx);
            cx.new(|cx| Root::new(workspace, window, cx))
        },
    )?;

    WorkspaceWindowHandle::from_root(root, cx)
        .ok_or_else(|| anyhow!("new Root window does not host a Workspace"))
}

fn open_reader_session_window(session: ReaderWindowSession, cx: &mut App) -> Result<usize> {
    let window_bounds = reader_window_bounds(session.bounds);
    let window_settings = cx.global::<SettingsStore>().settings.window;
    let transparency_enabled = session.transparency_enabled;
    let root = cx.open_window(
        WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(window_bounds),
            window_min_size: Some(size(
                px(READER_WINDOW_MIN_WIDTH),
                px(READER_WINDOW_MIN_HEIGHT),
            )),
            window_background: window_settings.background_appearance(transparency_enabled),
            ..Default::default()
        },
        move |window, cx| {
            window.set_app_id(APP_ID);
            let workspace = cx.new(|cx| {
                Workspace::from_reader_session(session.clone(), workspace_callbacks(), window, cx)
            });
            observe_reader_workspace(&workspace, window, cx);
            cx.new(|cx| Root::new(workspace, window, cx))
        },
    )?;
    let workspace = WorkspaceWindowHandle::from_root(root, cx)
        .ok_or_else(|| anyhow!("restored Root window does not host a Workspace"))?;
    workspace.update(cx, |workspace, _, _| {
        workspace.restored_missing_document_count()
    })
}

fn reader_window_bounds(bounds: ReaderWindowBounds) -> WindowBounds {
    let state = bounds.state;
    let bounds = Bounds {
        origin: point(px(bounds.x), px(bounds.y)),
        size: size(px(bounds.width), px(bounds.height)),
    };
    match state {
        ReaderWindowState::Windowed => WindowBounds::Windowed(bounds),
        ReaderWindowState::Maximized => WindowBounds::Maximized(bounds),
        ReaderWindowState::Fullscreen => WindowBounds::Fullscreen(bounds),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path, time::SystemTime};

    use glyphst_library::{
        ReaderPaneSession, ReaderPaneTree, ReaderSplitAxis, ReaderTabKind, ReaderTabSession,
        ReaderTranslationSession,
    };
    use gpui::{TestAppContext, VisualTestContext};

    fn temporary_pdf_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glyphst-reader-{label}-{}-{}.pdf",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write_minimal_pdf(path: &Path) {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>",
            "<< /Length 0 >>\nstream\n\nendstream",
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        fs::write(path, bytes).unwrap();
    }

    fn translation_reader_session(path: &Path) -> ReaderWindowSession {
        let document_tab = |id| ReaderTabSession {
            id,
            kind: ReaderTabKind::Document {
                path: path.to_path_buf(),
                position: DocumentPosition::fixed(0.0, 0.0, 1.0, 1),
            },
        };
        ReaderWindowSession {
            bounds: ReaderWindowBounds {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 800.0,
                state: ReaderWindowState::Windowed,
            },
            pane_tree: ReaderPaneTree::Split {
                axis: ReaderSplitAxis::Horizontal,
                ratios: vec![0.5, 0.5],
                children: vec![
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 1,
                        tabs: vec![document_tab(1)],
                        active_tab: Some(1),
                    }),
                    ReaderPaneTree::Pane(ReaderPaneSession {
                        id: 2,
                        tabs: vec![document_tab(2)],
                        active_tab: Some(2),
                    }),
                ],
            },
            active_pane: 1,
            dock_document_tab: Some(1),
            left_sidebar: None,
            ai_panel_open: false,
            transparency_enabled: false,
            synchronized_groups: Vec::new(),
            translation_sessions: vec![ReaderTranslationSession {
                source_tab: 1,
                translated_tab: 2,
                source_language: Some("en".to_string()),
                target_language: "zh-hans".to_string(),
                mode: "auto".to_string(),
                cache_key: None,
                original_unit_ids: vec!["unit-1".to_string()],
            }],
        }
    }

    #[test]
    fn startup_session_restore_is_bypassed_by_paths_and_new_windows() {
        let empty = instance::OpenRequest {
            new: false,
            path: None,
        };
        assert!(should_restore_reader_session(
            &empty,
            RestoreOnStartup::LastSession
        ));
        assert!(!should_restore_reader_session(
            &empty,
            RestoreOnStartup::None
        ));
        assert!(!should_restore_reader_session(
            &instance::OpenRequest {
                new: true,
                path: None,
            },
            RestoreOnStartup::LastSession,
        ));
        assert!(!should_restore_reader_session(
            &instance::OpenRequest {
                new: false,
                path: Some(PathBuf::from("book.pdf")),
            },
            RestoreOnStartup::LastSession,
        ));
    }

    #[test]
    fn restored_window_bounds_preserve_window_state() {
        let bounds = ReaderWindowBounds {
            x: -10.0,
            y: 20.0,
            width: 1200.0,
            height: 800.0,
            state: ReaderWindowState::Maximized,
        };
        assert!(matches!(
            reader_window_bounds(bounds),
            WindowBounds::Maximized(_)
        ));
    }

    #[gpui::test]
    fn final_window_close_flushes_live_translation_before_debounce(cx: &mut TestAppContext) {
        let document_path = temporary_pdf_path("close-live-translation");
        write_minimal_pdf(&document_path);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
            cx.set_global(RenderCache::new());
            let persistence = cx.new(|_| ReaderSessionPersistenceState {
                // Keep ordinary observation disabled so only the pre-close
                // flush can write the regression snapshot.
                suspended: true,
                generation: 0,
                save_task: None,
            });
            cx.set_global(ReaderSessionPersistence(persistence));
        });

        let session = translation_reader_session(&document_path);
        let root = cx.update(|cx| {
            let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                move |window, cx| {
                    let workspace = cx.new(|cx| {
                        Workspace::from_reader_session(session, workspace_callbacks(), window, cx)
                    });
                    observe_reader_workspace(&workspace, window, cx);
                    cx.new(|cx| Root::new(workspace, window, cx))
                },
            )
            .unwrap()
        });
        cx.run_until_parked();

        let workspace = cx.read(|cx| WorkspaceWindowHandle::from_root(root, cx).unwrap());
        workspace
            .update(cx, |workspace, _, cx| {
                assert!(!workspace.reader_session_restore_pending());
                // Model the manager's frequent progress notifications that kept
                // resetting the normal trailing-edge debounce.
                for _ in 0..4 {
                    cx.notify();
                }
            })
            .unwrap();
        assert!(cx.read(|cx| cx.global::<LibraryStore>().reader_session().is_none()));

        // The headless platform represents a server-decorated Linux window,
        // where the compositor owns the title-bar close button. Simulate that
        // platform close request and exercise the same production pre-close
        // hook that a real compositor invokes.
        let mut visual = VisualTestContext::from_window(root.into(), cx);
        assert!(visual.simulate_close());

        cx.read(|cx| {
            let snapshot = cx
                .global::<LibraryStore>()
                .reader_session()
                .expect("pre-close hook should persist the final reader window");
            assert_eq!(snapshot.windows.len(), 1);
            let window = &snapshot.windows[0];
            assert_eq!(window.translation_sessions.len(), 1);
            assert_eq!(window.translation_sessions[0].source_tab, 1);
            assert_eq!(window.translation_sessions[0].translated_tab, 2);
            let ReaderPaneTree::Split { children, .. } = &window.pane_tree else {
                panic!("translation session should preserve its split panes");
            };
            assert_eq!(children.len(), 2);
        });

        fs::remove_file(document_path).ok();
    }
}
