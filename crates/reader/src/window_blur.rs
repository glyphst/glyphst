//! Blur behind a transparent window, asked of the compositor through the
//! `ext-background-effect-v1` Wayland protocol (Niri and KWin offer it).
//! winit 0.30's `set_blur` speaks only KDE's older protocol, so this binds
//! the new one itself, on winit's own Wayland connection but with its own
//! event queue, and asks for the whole window to be blurred behind.

/// Whether the compositor can blur behind the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Support {
    /// Not asked yet (before the first frame).
    #[default]
    Unknown,
    Unsupported,
    Supported,
}

impl Support {
    /// For the settings window: `None` until known.
    pub fn known(self) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::Unsupported => Some(false),
            Self::Supported => Some(true),
        }
    }
}

#[derive(Default)]
pub struct WindowBlur {
    support: Support,
    #[cfg(target_os = "linux")]
    wayland: Option<wayland::Blur>,
    /// The blur region last asked for: the window's size in points, or
    /// `None` for no blur.
    applied: Option<Option<[i32; 2]>>,
}

impl WindowBlur {
    pub fn support(&self) -> Support {
        self.support
    }

    /// Asks for the window to be blurred behind (`wanted`) or not, once a
    /// frame; `size` is the window's in points. Attaches on the first call.
    pub fn update(&mut self, frame: &eframe::Frame, wanted: bool, size: egui::Vec2) {
        if self.support == Support::Unknown {
            self.attach(frame);
        }
        if self.support != Support::Supported {
            return;
        }
        let region = wanted.then(|| region_size(size));
        if self.applied == Some(region) {
            return;
        }
        #[cfg(target_os = "linux")]
        if let Some(blur) = &self.wayland {
            blur.set(region);
        }
        self.applied = Some(region);
    }

    #[cfg(target_os = "linux")]
    fn attach(&mut self, frame: &eframe::Frame) {
        self.wayland = wayland::Blur::attach(frame);
        self.support = if self.wayland.is_some() {
            Support::Supported
        } else {
            Support::Unsupported
        };
    }

    #[cfg(not(target_os = "linux"))]
    fn attach(&mut self, _frame: &eframe::Frame) {
        self.support = Support::Unsupported;
    }

    /// Lets go of the blur before the window goes: the protocol forbids
    /// using it once the surface is destroyed.
    pub fn detach(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(blur) = self.wayland.take() {
            blur.destroy();
        }
        self.applied = None;
    }
}

/// The blur region for a window `size` points large: whole points, at
/// least one.
fn region_size(size: egui::Vec2) -> [i32; 2] {
    [size.x, size.y].map(|side| (side.ceil() as i32).max(1))
}

/// Whether the manager's `capabilities` flags offer blur.
#[cfg(any(target_os = "linux", test))]
fn offers_blur(flags: u32) -> bool {
    flags & 1 != 0
}

#[cfg(target_os = "linux")]
mod wayland {
    use raw_window_handle::{
        HasDisplayHandle as _, HasWindowHandle as _, RawDisplayHandle, RawWindowHandle,
    };
    use wayland_client::{
        Connection, Dispatch, EventQueue, Proxy as _, QueueHandle, WEnum,
        backend::{Backend, ObjectId},
        globals::{GlobalListContents, registry_queue_init},
        protocol::{
            wl_compositor::WlCompositor, wl_region::WlRegion, wl_registry::WlRegistry,
            wl_surface::WlSurface,
        },
    };
    use wayland_protocols::ext::background_effect::v1::client::{
        ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
        ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
    };

    /// What the manager's events told.
    #[derive(Default)]
    struct State {
        capabilities: u32,
    }

    pub struct Blur {
        connection: Connection,
        queue: EventQueue<State>,
        compositor: WlCompositor,
        effect: ExtBackgroundEffectSurfaceV1,
    }

    impl Blur {
        /// Binds the protocol for the window `frame` draws; `None` off
        /// Wayland, or when the compositor doesn't offer blur.
        pub fn attach(frame: &eframe::Frame) -> Option<Self> {
            let RawDisplayHandle::Wayland(display) = frame.display_handle().ok()?.as_raw() else {
                return None;
            };
            let RawWindowHandle::Wayland(window) = frame.window_handle().ok()?.as_raw() else {
                return None;
            };
            // SAFETY: the pointer is winit's live `wl_display`, which
            // outlives the app; a backend from a foreign display never
            // closes it. (One of this file's two unsafe calls: there is no
            // safe way onto winit's connection.)
            #[allow(unsafe_code)]
            let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
            let connection = Connection::from_backend(backend);
            let (globals, mut queue) = registry_queue_init::<State>(&connection).ok()?;
            let qh = queue.handle();
            let compositor: WlCompositor = globals.bind(&qh, 1..=4, ()).ok()?;
            let manager: ExtBackgroundEffectManagerV1 = globals.bind(&qh, 1..=1, ()).ok()?;
            let mut state = State::default();
            // The manager sends its capabilities when bound.
            queue.roundtrip(&mut state).ok()?;
            if !super::offers_blur(state.capabilities) {
                manager.destroy();
                return None;
            }
            // SAFETY: the pointer is the window's live `wl_surface` on this
            // display; the effect made on it is destroyed in `on_exit`,
            // before eframe destroys the surface.
            #[allow(unsafe_code)]
            let id = unsafe {
                ObjectId::from_ptr(WlSurface::interface(), window.surface.as_ptr().cast())
            }
            .ok()?;
            let surface = WlSurface::from_id(&connection, id).ok()?;
            let effect = manager.get_background_effect(&surface, &qh, ());
            manager.destroy();
            connection.flush().ok()?;
            Some(Self {
                connection,
                queue,
                compositor,
                effect,
            })
        }

        /// Blurs behind the window's `[width, height]` points, or nothing;
        /// the window's next frame applies it.
        pub fn set(&self, region: Option<[i32; 2]>) {
            let qh = self.queue.handle();
            match region {
                Some([width, height]) => {
                    let region = self.compositor.create_region(&qh, ());
                    region.add(0, 0, width, height);
                    self.effect.set_blur_region(Some(&region));
                    region.destroy();
                }
                None => self.effect.set_blur_region(None),
            }
            let _ = self.connection.flush();
        }

        pub fn destroy(self) {
            self.effect.destroy();
            let _ = self.connection.flush();
        }
    }

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as wayland_client::Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ExtBackgroundEffectManagerV1, ()> for State {
        fn event(
            state: &mut Self,
            _: &ExtBackgroundEffectManagerV1,
            event: ext_background_effect_manager_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let ext_background_effect_manager_v1::Event::Capabilities {
                flags: WEnum::Value(flags),
            } = event
            {
                state.capabilities = flags.bits();
            }
        }
    }

    wayland_client::delegate_noop!(State: ignore WlCompositor);
    wayland_client::delegate_noop!(State: ignore WlRegion);
    wayland_client::delegate_noop!(State: ignore ExtBackgroundEffectSurfaceV1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_is_offered_by_its_capability_bit() {
        assert!(offers_blur(1));
        assert!(offers_blur(3));
        assert!(!offers_blur(0));
        assert!(!offers_blur(2));
    }

    #[test]
    fn the_region_covers_the_window_in_whole_points() {
        assert_eq!(region_size(egui::vec2(1440.4, 919.6)), [1441, 920]);
        assert_eq!(region_size(egui::vec2(0.0, -3.0)), [1, 1]);
    }

    #[test]
    fn a_blur_never_attached_is_inert() {
        let mut blur = WindowBlur::default();
        assert_eq!(blur.support(), Support::Unknown);
        assert_eq!(blur.support().known(), None);
        blur.detach();
        assert_eq!(Support::Unsupported.known(), Some(false));
        assert_eq!(Support::Supported.known(), Some(true));
    }
}
