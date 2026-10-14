//! Finding agent CLIs. A Reader started from the desktop often has a short
//! PATH, so the login shell's PATH and the usual install folders are
//! searched too.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

/// How long the login shell may take to print its PATH.
const SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// The folders searched for programs, in order.
pub fn search_path() -> Vec<PathBuf> {
    let mut paths = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    for path in login_shell_path().iter().chain(&install_folders()) {
        if !paths.contains(path) {
            paths.push(path.clone());
        }
    }
    paths
}

/// `name` on the search path, or the explicit `configured` path.
pub fn find(name: &str, configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured.filter(|path| !path.as_os_str().is_empty()) {
        return path.is_file().then(|| path.to_path_buf());
    }
    search_path()
        .into_iter()
        .map(|folder| folder.join(name))
        .find(|candidate| candidate.is_file())
}

/// The first line of `program --version`.
pub fn version(program: &Path) -> Option<String> {
    let output = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

fn install_folders() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let user = std::env::var("USER").unwrap_or_default();
    [
        home.join(".local/bin"),
        home.join(".claude/local"),
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        home.join(".cargo/bin"),
        home.join(".nix-profile/bin"),
        PathBuf::from(format!("/etc/profiles/per-user/{user}/bin")),
        PathBuf::from("/run/current-system/sw/bin"),
        PathBuf::from("/usr/local/bin"),
    ]
    .into_iter()
    .collect()
}

/// The login shell's PATH, read once.
fn login_shell_path() -> &'static [PathBuf] {
    static PATH: OnceLock<Vec<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let Ok(mut child) = Command::new(&shell)
            .args(["-l", "-c", "printf %s \"$PATH\""])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return Vec::new();
        };
        let deadline = Instant::now() + SHELL_TIMEOUT;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                let Ok(output) = child.wait_with_output() else {
                    return Vec::new();
                };
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                return std::env::split_paths(&path).collect();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_found_on_the_search_path() {
        let sh = find("sh", None).expect("sh is installed");
        assert!(sh.is_file());
        assert_eq!(find("sh", Some(Path::new("/nonexistent/sh"))), None);
        assert_eq!(find("glyphst-no-such-program", None), None);
    }
}
