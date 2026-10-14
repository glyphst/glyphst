//! Each document's working folder for external agents, a git repository
//! whose checkpoints (taken without touching the agent's own git state, as
//! Zed does) let the user undo what an agent wrote.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result, anyhow, bail};

/// A document's working folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scratch {
    pub dir: PathBuf,
}

/// Files that changed between two checkpoints.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileChanges {
    pub before: String,
    pub after: String,
    /// `git diff --stat`-like lines: "path | +3 -1".
    pub files: Vec<String>,
}

impl Scratch {
    /// A stable folder name for the document at `path`: its name, and a
    /// hash of its path.
    pub fn key(path: &Path) -> String {
        use sha2::{Digest as _, Sha256};

        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
            .chars()
            .map(|ch| if ch.is_alphanumeric() { ch } else { '-' })
            .take(40)
            .collect::<String>();
        let digest = Sha256::digest(path.to_string_lossy().as_bytes());
        let hash = digest
            .iter()
            .take(6)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("{stem}-{hash}")
    }

    /// The folder for the document `key` (a stable name) under `root`,
    /// made a git repository when git is installed.
    pub fn open(root: &Path, key: &str, title: &str) -> Result<Self> {
        let dir = root.join(key);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        let readme = dir.join("README.md");
        if !readme.exists() {
            std::fs::write(
                &readme,
                format!(
                    "# {title}\n\nGlyphst Reader keeps an agent's files for this document here. The document itself is read through the Reader's `glyphst` MCP tools.\n"
                ),
            )?;
        }
        let scratch = Self { dir };
        if git_installed() && !scratch.dir.join(".git").exists() {
            scratch.git(&["init", "--quiet"])?;
        }
        Ok(scratch)
    }

    pub fn has_git(&self) -> bool {
        self.dir.join(".git").is_dir() && git_installed()
    }

    /// Records the folder's files as a commit object (not on any branch)
    /// and returns its id.
    pub fn checkpoint(&self) -> Result<String> {
        if !self.has_git() {
            bail!("the working folder has no git repository");
        }
        let index = self.temporary_index();
        let result = (|| {
            self.git_with_index(&index, &["add", "--all", "--force", "."])?;
            let tree = self.git_with_index(&index, &["write-tree"])?;
            let commit = self.git(&[
                "-c",
                "user.name=Glyphst",
                "-c",
                "user.email=glyphst@localhost",
                "commit-tree",
                tree.trim(),
                "-m",
                "Glyphst checkpoint",
            ])?;
            let commit = commit.trim().to_string();
            // A ref keeps the checkpoint from garbage collection.
            self.git(&["update-ref", &format!("refs/glyphst/{commit}"), &commit])?;
            Ok(commit)
        })();
        let _ = std::fs::remove_file(&index);
        result
    }

    /// The tree a checkpoint recorded.
    fn tree_of(&self, checkpoint: &str) -> Result<String> {
        Ok(self
            .git(&["rev-parse", &format!("{checkpoint}^{{tree}}")])?
            .trim()
            .to_string())
    }

    /// Whether the folder still holds what `checkpoint` recorded.
    pub fn matches(&self, checkpoint: &str) -> Result<bool> {
        let now = self.checkpoint()?;
        Ok(self.tree_of(&now)? == self.tree_of(checkpoint)?)
    }

    /// The files that changed between two checkpoints.
    pub fn changes(&self, before: &str, after: &str) -> Result<FileChanges> {
        let stat = self.git(&["diff", "--numstat", before, after])?;
        let files = stat
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(3, '\t');
                let (added, removed, path) = (parts.next()?, parts.next()?, parts.next()?);
                Some(format!("{path} | +{added} -{removed}"))
            })
            .collect();
        Ok(FileChanges {
            before: before.to_string(),
            after: after.to_string(),
            files,
        })
    }

    /// Puts the folder back as `checkpoint` recorded it, removing files it
    /// did not have.
    pub fn restore(&self, checkpoint: &str) -> Result<()> {
        let index = self.temporary_index();
        let result = (|| {
            self.git_with_index(&index, &["read-tree", checkpoint])?;
            let extra = self.git_with_index(
                &index,
                &["ls-files", "--others", "-z", "--exclude-standard"],
            )?;
            for path in extra.split('\0').filter(|path| !path.is_empty()) {
                let path = self.dir.join(path);
                if path.starts_with(&self.dir) {
                    let _ = std::fs::remove_file(&path);
                }
            }
            self.git_with_index(&index, &["checkout-index", "--all", "--force"])?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&index);
        result
    }

    fn temporary_index(&self) -> PathBuf {
        self.dir
            .join(".git")
            .join(format!("glyphst-index-{}", uuid::Uuid::new_v4().simple()))
    }

    fn git(&self, arguments: &[&str]) -> Result<String> {
        run_git(&self.dir, arguments, None)
    }

    fn git_with_index(&self, index: &Path, arguments: &[&str]) -> Result<String> {
        run_git(&self.dir, arguments, Some(index))
    }
}

fn git_installed() -> bool {
    Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_git(dir: &Path, arguments: &[&str], index: Option<&Path>) -> Result<String> {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(arguments)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command
        .output()
        .with_context(|| format!("could not run git {}", arguments.join(" ")))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_get_stable_folder_names() {
        let key = Scratch::key(Path::new("/books/A Paper (2024).pdf"));
        assert!(key.starts_with("A-Paper--2024--"), "{key}");
        assert_eq!(key, Scratch::key(Path::new("/books/A Paper (2024).pdf")));
        assert_ne!(key, Scratch::key(Path::new("/other/A Paper (2024).pdf")));
    }

    #[test]
    fn checkpoints_restore_what_agents_wrote() {
        let root = std::env::temp_dir().join(format!(
            "glyphst-scratch-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let scratch = Scratch::open(&root, "doc", "A paper").unwrap();
        assert!(scratch.has_git());
        let before = scratch.checkpoint().unwrap();
        assert!(scratch.matches(&before).unwrap());

        std::fs::write(scratch.dir.join("notes.md"), "one\ntwo\n").unwrap();
        std::fs::write(scratch.dir.join("README.md"), "changed\n").unwrap();
        let after = scratch.checkpoint().unwrap();
        assert!(!scratch.matches(&before).unwrap());
        let changes = scratch.changes(&before, &after).unwrap();
        assert_eq!(changes.files, ["README.md | +1 -3", "notes.md | +2 -0"]);

        scratch.restore(&before).unwrap();
        assert!(!scratch.dir.join("notes.md").exists());
        assert!(
            std::fs::read_to_string(scratch.dir.join("README.md"))
                .unwrap()
                .contains("A paper")
        );
        scratch.restore(&after).unwrap();
        assert_eq!(
            std::fs::read_to_string(scratch.dir.join("notes.md")).unwrap(),
            "one\ntwo\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
