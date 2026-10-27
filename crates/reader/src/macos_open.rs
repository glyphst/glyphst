//! Documents and links macOS hands the Reader: Open With in Finder, a
//! double-click on a document the Reader opens by default, a `glyphst://`
//! link. macOS sends these as Apple Events rather than arguments, to the
//! running Reader or to the one it launches for them. winit leaves the
//! application delegate to the app; ours queues what arrives until the
//! Reader takes it between frames.

use std::{
    path::PathBuf,
    sync::{Mutex, PoisonError},
};

use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
};
use objc2_app_kit::{NSApplication, NSApplicationDelegate};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSURL};

/// What arrived and not yet taken, and the context to wake.
struct Arrivals {
    files: Vec<PathBuf>,
    urls: Vec<String>,
    ctx: Option<egui::Context>,
}

static ARRIVALS: Mutex<Arrivals> = Mutex::new(Arrivals {
    files: Vec::new(),
    urls: Vec::new(),
    ctx: None,
});

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the delegate
    // does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlyphstReaderAppDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(application:openURLs:))]
        fn application_open_urls(&self, _application: &NSApplication, urls: &NSArray<NSURL>) {
            let mut arrivals = ARRIVALS.lock().unwrap_or_else(PoisonError::into_inner);
            for url in urls.iter() {
                if url.isFileURL() {
                    if let Some(path) = url.path() {
                        arrivals.files.push(PathBuf::from(path.to_string()));
                    }
                } else if let Some(link) = url.absoluteString().map(|link| link.to_string())
                    && link.starts_with(crate::links::SCHEME)
                {
                    arrivals.urls.push(link);
                }
            }
            if let Some(ctx) = &arrivals.ctx {
                ctx.request_repaint();
            }
        }
    }
);

impl Delegate {
    // This file's one unsafe call: objc2 has no safe init for a defined
    // class.
    #[allow(unsafe_code)]
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initialiser, on a freshly allocated
        // instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// Makes ours the application's delegate. Call after winit's event loop
/// exists (it sets up the application) and before it runs, so documents
/// the Reader was launched for arrive too.
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("documents from Finder need the main thread");
        return;
    };
    let delegate = Delegate::new(mtm);
    NSApplication::sharedApplication(mtm).setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The application holds its delegate weakly; this one lives as long as
    // the process.
    std::mem::forget(delegate);
}

/// The context to repaint when something arrives.
pub fn set_context(ctx: &egui::Context) {
    ARRIVALS.lock().unwrap_or_else(PoisonError::into_inner).ctx = Some(ctx.clone());
}

/// The documents and links that arrived since the last call.
pub fn take() -> (Vec<PathBuf>, Vec<String>) {
    let mut arrivals = ARRIVALS.lock().unwrap_or_else(PoisonError::into_inner);
    (
        std::mem::take(&mut arrivals.files),
        std::mem::take(&mut arrivals.urls),
    )
}
