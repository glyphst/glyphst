//! The desktop's light or dark choice, for the System theme, after
//! pdfcraft's `desktop_theme.rs`.
//!
//! winit reports no system theme on Linux (X11 never does; Wayland only
//! with client-side decorations, which Niri and most tiling compositors
//! don't draw), so the System theme asks the desktop itself: the XDG
//! Desktop Portal's Settings interface answers on GNOME, KDE, Niri and
//! others, over X11 and Wayland alike.

use std::{
    sync::mpsc::{Receiver, TryRecvError, channel},
    time::Duration,
};

/// How often the watcher re-reads the desktop's answer. The portal also
/// signals changes, but one round trip every few seconds is cheap and
/// soon enough after a theme switch.
const POLL: Duration = Duration::from_secs(4);

/// Watches the desktop and reports its choice when it changes.
#[derive(Default)]
pub struct DesktopTheme {
    /// `None` when no watcher thread could be started.
    changes: Option<Receiver<Option<bool>>>,
}

impl DesktopTheme {
    /// Starts watching; the first answer is sent straight away. `wake`
    /// runs after each change, to repaint the app.
    pub fn start(wake: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let started = std::thread::Builder::new()
            .name("glyphst-desktop-theme".into())
            .spawn(move || {
                let mut portal = Portal::default();
                let mut last = portal.prefers_dark();
                if tx.send(last).is_err() {
                    return;
                }
                wake();
                loop {
                    std::thread::sleep(POLL);
                    let now = portal.prefers_dark();
                    if now != last {
                        last = now;
                        if tx.send(now).is_err() {
                            return;
                        }
                        wake();
                    }
                }
            });
        Self {
            changes: started.ok().map(|_| rx),
        }
    }

    /// The desktop's new answer if it changed since the last call: whether
    /// it asks for dark, `None` inside when it states no preference.
    /// Several changes collapse to the newest.
    pub fn take_change(&mut self) -> Option<Option<bool>> {
        let changes = self.changes.as_ref()?;
        let mut newest = None;
        loop {
            match changes.try_recv() {
                Ok(changed) => newest = Some(changed),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return newest,
            }
        }
    }
}

/// A session bus connection, opened on first use and again after an error.
#[derive(Default)]
struct Portal {
    #[cfg(target_os = "linux")]
    connection: Option<zbus::Connection>,
}

impl Portal {
    /// Whether the desktop asks applications to draw dark; `None` when it
    /// says nothing or there is no portal to ask. D-Bus calls block, so
    /// this belongs on the watcher's thread.
    #[cfg(target_os = "linux")]
    fn prefers_dark(&mut self) -> Option<bool> {
        const PORTAL: &str = "org.freedesktop.portal.Desktop";
        const PATH: &str = "/org/freedesktop/portal/desktop";
        const SETTINGS: &str = "org.freedesktop.portal.Settings";
        // The freedesktop key first (a number), then the GNOME schema's
        // string, which XFCE's portal proxies.
        const KEYS: [(&str, &str); 2] = [
            ("org.freedesktop.appearance", "color-scheme"),
            ("org.gnome.desktop.interface", "color-scheme"),
        ];
        if self.connection.is_none() {
            self.connection = pollster::block_on(zbus::Connection::session()).ok();
        }
        let connection = self.connection.as_ref()?;
        let mut reached = false;
        for (namespace, key) in KEYS {
            let reply = pollster::block_on(connection.call_method(
                Some(PORTAL),
                PATH,
                Some(SETTINGS),
                "ReadOne",
                &(namespace, key),
            ));
            let Ok(reply) = reply else { continue };
            reached = true;
            let Ok((value,)) = reply.body().deserialize::<(zbus::zvariant::OwnedValue,)>() else {
                continue;
            };
            if let Some(answer) = parse(&value) {
                return answer;
            }
        }
        if !reached {
            // The bus went away (or never had a portal): reconnect next time.
            self.connection = None;
        }
        None
    }

    /// macOS and Windows answer through winit.
    #[cfg(not(target_os = "linux"))]
    fn prefers_dark(&mut self) -> Option<bool> {
        None
    }
}

/// A portal answer: `Some(choice)` when it is a colour scheme, the choice
/// `None` for "no preference"; `None` when it is something else.
#[cfg(target_os = "linux")]
fn parse(value: &zbus::zvariant::Value<'_>) -> Option<Option<bool>> {
    use zbus::zvariant::Value;
    match value {
        // Older portals wrap the answer in a second variant.
        Value::Value(inner) => parse(inner),
        Value::U32(scheme) => Some(scheme_number(*scheme)),
        Value::Str(scheme) => Some(scheme_name(scheme.as_str())),
        _ => None,
    }
}

/// `org.freedesktop.appearance color-scheme`: 1 prefers dark, 2 light, 0
/// (or anything newer) states no preference.
#[cfg(any(target_os = "linux", test))]
fn scheme_number(scheme: u32) -> Option<bool> {
    match scheme {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// The GNOME schema's `color-scheme`; `default` states no preference.
#[cfg(any(target_os = "linux", test))]
fn scheme_name(scheme: &str) -> Option<bool> {
    match scheme {
        "prefer-dark" => Some(true),
        "prefer-light" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_answers_read_as_a_choice_or_no_preference() {
        assert_eq!(scheme_number(1), Some(true));
        assert_eq!(scheme_number(2), Some(false));
        assert_eq!(scheme_number(0), None);
        assert_eq!(scheme_number(7), None);
        assert_eq!(scheme_name("prefer-dark"), Some(true));
        assert_eq!(scheme_name("prefer-light"), Some(false));
        assert_eq!(scheme_name("default"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wrapped_and_unexpected_values_parse() {
        use zbus::zvariant::Value;
        assert_eq!(parse(&Value::U32(1)), Some(Some(true)));
        assert_eq!(
            parse(&Value::Value(Box::new(Value::U32(2)))),
            Some(Some(false))
        );
        assert_eq!(parse(&Value::from("default")), Some(None));
        assert_eq!(parse(&Value::Bool(true)), None);
    }

    #[test]
    fn a_watcher_without_a_thread_is_inert() {
        assert_eq!(DesktopTheme::default().take_change(), None);
    }
}
