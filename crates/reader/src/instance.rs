//! One running Reader for links: a `glyphst://` citation clicked in another
//! app (Obsidian, a browser) starts `glyphst-reader glyphst://…`, which
//! hands the link to the Reader already running over a local socket and
//! exits. The Reader takes links as they come, between frames.
//!
//! Registering the handler writes a desktop entry for
//! `x-scheme-handler/glyphst` (Linux). On macOS the app bundle declares the
//! scheme and links arrive through `macos_open`; on Windows the installer
//! registers it, and each link starts a Reader of its own.

use std::{
    io,
    path::{Path, PathBuf},
    sync::mpsc::Receiver,
};

/// The running Reader's socket: in the user's runtime folder (private to
/// them), else in the state folder.
pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(glyphst_core::paths::state_dir)
        .join("glyphst-reader.sock")
}

/// Hands `urls` to the Reader listening at `path`: true when one took them,
/// false when none runs.
#[cfg(unix)]
pub fn forward(path: &Path, urls: &[String]) -> io::Result<bool> {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixStream;
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    for url in urls {
        writeln!(stream, "open {}", url.replace(['\n', '\r'], ""))?;
    }
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    Ok(answer.trim() == "ok")
}

#[cfg(not(unix))]
pub fn forward(_path: &Path, _urls: &[String]) -> io::Result<bool> {
    Ok(false)
}

/// The running Reader's end: links other processes forwarded.
pub struct Instance {
    links: Receiver<String>,
    path: PathBuf,
}

impl Instance {
    /// Listens at `path`, replacing a socket no Reader answers on; fails
    /// when another Reader listens there already.
    #[cfg(unix)]
    pub fn listen(path: &Path, ctx: &egui::Context) -> io::Result<Self> {
        use std::io::{BufRead as _, BufReader, Write as _};
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::mpsc;
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another Reader takes the links",
                ));
            }
            // Left by a Reader that ended without cleaning up.
            std::fs::remove_file(path)?;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(path)?;
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let (sender, links) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("links".to_string())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else {
                        continue;
                    };
                    let reader = BufReader::new(&stream);
                    for line in reader.lines().map_while(Result::ok) {
                        if let Some(url) = line.strip_prefix("open ")
                            && url.starts_with(crate::links::SCHEME)
                            && sender.send(url.to_string()).is_err()
                        {
                            return;
                        }
                    }
                    let _ = (&stream).write_all(b"ok\n");
                    ctx.request_repaint();
                }
            })?;
        Ok(Self {
            links,
            path: path.to_path_buf(),
        })
    }

    #[cfg(not(unix))]
    pub fn listen(_path: &Path, _ctx: &egui::Context) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "links from other apps are not taken here yet",
        ))
    }

    /// The links that came since the last call.
    pub fn take(&self) -> Vec<String> {
        self.links.try_iter().collect()
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The desktop entry's file name, beside the Reader's own.
pub const HANDLER_ENTRY: &str = "glyphst-reader-url.desktop";

/// The desktop entry that makes `exe` open `glyphst://` links.
pub fn handler_entry(exe: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Glyphst Reader (links)\n\
         Comment=Open glyphst:// citations in Glyphst Reader\n\
         Exec={exe} %u\n\
         Icon=x-office-document\n\
         NoDisplay=true\n\
         Terminal=false\n\
         MimeType=x-scheme-handler/glyphst;\n\
         StartupWMClass=glyphst-reader\n"
    )
}

/// Which app opens `glyphst://` links now, as `xdg-mime` says (Linux).
pub fn handler() -> Option<String> {
    let output = std::process::Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/glyphst"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Makes this Reader the handler: a desktop entry in the user's
/// applications folder (naming the `glyphst-reader` on `PATH`, which
/// outlives an update, else this executable), then `xdg-mime default`.
pub fn register() -> anyhow::Result<()> {
    use anyhow::Context as _;
    let exe = which("glyphst-reader")
        .or_else(|| std::env::current_exe().ok())
        .context("could not find glyphst-reader")?;
    let folder = dirs::data_dir()
        .context("no data folder")?
        .join("applications");
    std::fs::create_dir_all(&folder)?;
    let exe = exe.display().to_string();
    let exe = if exe.contains(' ') {
        format!("\"{exe}\"")
    } else {
        exe
    };
    std::fs::write(folder.join(HANDLER_ENTRY), handler_entry(&exe))?;
    let status = std::process::Command::new("xdg-mime")
        .args(["default", HANDLER_ENTRY, "x-scheme-handler/glyphst"])
        .status()
        .context("could not run xdg-mime")?;
    anyhow::ensure!(status.success(), "xdg-mime failed ({status})");
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&folder)
        .status();
    Ok(())
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn links_reach_the_running_reader_and_a_stale_socket_is_replaced() {
        let path =
            std::env::temp_dir().join(format!("glyphst-instance-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(
            !forward(&path, &["glyphst://open?id=1".to_string()]).unwrap(),
            "none runs"
        );

        // A socket left by a Reader that crashed.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let ctx = egui::Context::default();
        let instance = Instance::listen(&path, &ctx).unwrap();
        assert!(
            Instance::listen(&path, &ctx).is_err(),
            "one Reader takes links"
        );

        let urls = [
            "glyphst://open?id=12&page=5&chunk=381".to_string(),
            "glyphst://open?id=7".to_string(),
        ];
        assert!(forward(&path, &urls).unwrap());
        assert_eq!(instance.take(), urls);
        // Only glyphst:// links are taken.
        assert!(forward(&path, &["file:///etc/passwd".to_string()]).unwrap());
        assert!(instance.take().is_empty());
        drop(instance);
        assert!(!path.exists());
    }

    #[test]
    fn the_handler_entry_names_the_reader_for_glyphst_links() {
        let entry = handler_entry("/usr/bin/glyphst-reader");
        assert!(entry.contains("Exec=/usr/bin/glyphst-reader %u\n"));
        assert!(entry.contains("MimeType=x-scheme-handler/glyphst;\n"));
        assert!(entry.contains("NoDisplay=true\n"));
    }
}
