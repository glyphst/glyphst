//! Keeping the library folder and a remote store (WebDAV, S3) the same,
//! for libraries no sync tool carries: what changed on one side since the
//! last run goes to the other, against a manifest of how both sides were
//! then (a three-way merge per file). A note changed on both sides keeps
//! both: the remote's under its name, this computer's beside it as
//! `name (conflict <computer> <date>).md`, uploaded too.
//!
//! The journal's files have one writer each and the library's copied
//! files are named by uuid, so only notes ever meet both ways.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context as _, Result};
use opendal::{Operator, layers::RetryLayer};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Files go up and down in parts this big.
const CHUNK: usize = 8 * 1024 * 1024;
const MANIFEST_FORMAT: u32 = 1;

/// Where a library is mirrored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Remote {
    S3 {
        endpoint: String,
        region: String,
        bucket: String,
        /// A folder in the bucket (`library/`), or empty.
        prefix: String,
        access_key_id: String,
        secret_access_key: String,
    },
    WebDav {
        /// The folder the library goes in, such as
        /// `https://cloud.example.org/remote.php/dav/files/me/Glyphst`.
        url: String,
        user: String,
        password: String,
    },
}

impl Remote {
    /// A name for the remote that changes when it does: its manifest's.
    pub fn key(&self) -> String {
        let text = match self {
            Self::S3 {
                endpoint,
                bucket,
                prefix,
                ..
            } => format!("s3:{endpoint}/{bucket}/{prefix}"),
            Self::WebDav { url, user, .. } => format!("webdav:{user}@{url}"),
        };
        hex(&Sha256::digest(text.as_bytes()))[..16].to_string()
    }

    /// [`Self::key`] for this store and a library folder: the name of the
    /// manifest of what the two held.
    pub fn key_for(&self, folder: &Path) -> String {
        let text = format!("{}\n{}", self.key(), folder.display());
        hex(&Sha256::digest(text.as_bytes()))[..16].to_string()
    }

    /// The store, retrying what fails on the way.
    pub fn operator(&self) -> Result<Operator> {
        // The process-wide HTTP transport (reqwest), once.
        opendal::install_default();
        let operator = match self {
            Self::S3 {
                endpoint,
                region,
                bucket,
                prefix,
                access_key_id,
                secret_access_key,
            } => {
                // Only what is given here: no ~/.aws files or environment.
                let builder = opendal::services::S3::default()
                    .endpoint(endpoint.trim())
                    .region(if region.trim().is_empty() {
                        "auto"
                    } else {
                        region.trim()
                    })
                    .bucket(bucket.trim())
                    .root(&format!("/{}", prefix.trim().trim_matches('/')))
                    .access_key_id(access_key_id.trim())
                    .secret_access_key(secret_access_key)
                    .disable_config_load();
                Operator::new(builder)?
            }
            Self::WebDav {
                url,
                user,
                password,
            } => {
                let (endpoint, root) = split_url(url);
                let builder = opendal::services::Webdav::default()
                    .endpoint(&endpoint)
                    .root(&root)
                    .username(user)
                    .password(password);
                Operator::new(builder)?
            }
        };
        Ok(operator.layer(RetryLayer::new().with_max_times(4)))
    }
}

/// `https://host/a/b` as the server (`https://host`) and the folder
/// (`/a/b`).
fn split_url(url: &str) -> (String, String) {
    let url = url.trim().trim_end_matches('/');
    let after_scheme = url.find("://").map_or(0, |at| at + 3);
    match url[after_scheme..].find('/') {
        Some(slash) => (
            url[..after_scheme + slash].to_string(),
            url[after_scheme + slash..].to_string(),
        ),
        None => (url.to_string(), "/".to_string()),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// How a file was on both sides after the last run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Base {
    /// The remote's etag, or its size and time where it gives none.
    remote: String,
    local_sha: String,
    local_size: u64,
    local_modified: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    format: u32,
    files: BTreeMap<String, Base>,
}

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MirrorReport {
    pub uploaded: Vec<String>,
    pub downloaded: Vec<String>,
    pub deleted_here: Vec<String>,
    pub deleted_there: Vec<String>,
    /// Notes changed on both sides: this computer's copy, under its new
    /// name.
    pub conflicts: Vec<String>,
}

impl MirrorReport {
    pub fn is_empty(&self) -> bool {
        self.uploaded.is_empty()
            && self.downloaded.is_empty()
            && self.deleted_here.is_empty()
            && self.deleted_there.is_empty()
            && self.conflicts.is_empty()
    }

    fn extend(&mut self, other: Self) {
        self.uploaded.extend(other.uploaded);
        self.downloaded.extend(other.downloaded);
        self.deleted_here.extend(other.deleted_here);
        self.deleted_there.extend(other.deleted_there);
        self.conflicts.extend(other.conflicts);
    }
}

/// Whether a file under the library folder stays on this computer.
fn excluded(relative: &str) -> bool {
    relative.split('/').any(|part| part.starts_with('.'))
        || relative.ends_with(".glyphst-writing")
        || relative.ends_with(".glyphst-moving")
}

/// The runtime every mirror's requests run on: one, with a thread of its
/// own, since the HTTP client's connections live on the runtime that
/// opened them.
fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let built = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("library-mirror")
        .enable_all()
        .build()?;
    Ok(RUNTIME.get_or_init(|| built))
}

/// The library folder mirrored to a remote.
pub struct Mirror {
    operator: Operator,
    runtime: &'static tokio::runtime::Runtime,
    folder: PathBuf,
    manifest_path: PathBuf,
    manifest: Manifest,
    /// This computer's name, for conflict copies.
    computer: String,
}

impl Mirror {
    /// `manifest_path` is this computer's, outside the library folder.
    pub fn new(
        operator: Operator,
        folder: &Path,
        manifest_path: &Path,
        computer: &str,
    ) -> Result<Self> {
        let runtime = runtime()?;
        let manifest = fs::read(manifest_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
            .filter(|manifest| manifest.format == MANIFEST_FORMAT)
            .unwrap_or(Manifest {
                format: MANIFEST_FORMAT,
                files: BTreeMap::new(),
            });
        Ok(Self {
            operator,
            runtime,
            folder: folder.to_path_buf(),
            manifest_path: manifest_path.to_path_buf(),
            manifest,
            computer: computer.to_string(),
        })
    }

    /// Writes, lists and removes a small file: whether the remote takes
    /// this library. Returns how many files it holds under `sync/`.
    pub fn probe(&self) -> Result<usize> {
        self.runtime.block_on(async {
            let probe = format!("sync/.probe-{}", uuid::Uuid::new_v4());
            self.operator
                .write(&probe, b"glyphst".to_vec())
                .await
                .context("writing a test file")?;
            let found = self
                .operator
                .list_with("sync/")
                .recursive(true)
                .await
                .context("listing")?;
            self.operator
                .delete(&probe)
                .await
                .context("removing the test file")?;
            Ok(found
                .iter()
                .filter(|entry| entry.metadata().is_file() && entry.path() != probe)
                .count())
        })
    }

    /// One run over `prefixes` (`sync/` first, then `files/`, `notes/`),
    /// keeping the manifest after each.
    pub fn run(&mut self, prefixes: &[&str]) -> Result<MirrorReport> {
        let mut report = MirrorReport::default();
        for prefix in prefixes {
            let result = self.run_prefix(prefix);
            self.save()?;
            report.extend(result?);
        }
        Ok(report)
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.manifest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.manifest_path.with_extension("json.writing");
        fs::write(&temporary, serde_json::to_vec(&self.manifest)?)?;
        fs::rename(&temporary, &self.manifest_path)?;
        Ok(())
    }

    /// The remote's files under `prefix`, with their change tokens.
    fn remote_files(&self, prefix: &str) -> Result<BTreeMap<String, String>> {
        let entries = self.runtime.block_on(async {
            match self.operator.list_with(prefix).recursive(true).await {
                Ok(entries) => Ok(entries),
                Err(error) if error.kind() == opendal::ErrorKind::NotFound => Ok(Vec::new()),
                Err(error) => Err(error),
            }
        })?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.metadata().is_file())
            .filter(|entry| !excluded(entry.path()))
            .map(|entry| {
                let metadata = entry.metadata();
                let token = metadata.etag().map(str::to_string).unwrap_or_else(|| {
                    format!(
                        "{}@{}",
                        metadata.content_length(),
                        metadata
                            .last_modified()
                            .map(|time| time.to_string())
                            .unwrap_or_default()
                    )
                });
                (entry.path().to_string(), token)
            })
            .collect())
    }

    /// The local files under `prefix`: their size and time.
    fn local_files(&self, prefix: &str) -> BTreeMap<String, (u64, u64)> {
        let mut found = BTreeMap::new();
        let mut folders = vec![self.folder.join(prefix.trim_end_matches('/'))];
        while let Some(folder) = folders.pop() {
            let Ok(entries) = fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_dir() {
                    folders.push(path);
                    continue;
                }
                let relative = relative(&self.folder, &path);
                if excluded(&relative) {
                    continue;
                }
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |time| time.as_nanos() as u64);
                found.insert(relative, (metadata.len(), modified));
            }
        }
        found
    }

    /// A local file's digest, from the manifest when its size and time
    /// have not moved.
    fn local_sha(&self, relative: &str, size: u64, modified: u64) -> Result<String> {
        if let Some(base) = self.manifest.files.get(relative)
            && base.local_size == size
            && base.local_modified == modified
        {
            return Ok(base.local_sha.clone());
        }
        let mut file = fs::File::open(self.folder.join(relative))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1 << 16];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex(&hasher.finalize()))
    }

    fn run_prefix(&mut self, prefix: &str) -> Result<MirrorReport> {
        let mut report = MirrorReport::default();
        let remote = self.remote_files(prefix)?;
        let local = self.local_files(prefix);
        let known = self
            .manifest
            .files
            .keys()
            .filter(|path| path.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        // A folder that lost everything it had (a disk not mounted, a
        // folder moved) does not empty the store.
        let based_here = self
            .manifest
            .files
            .keys()
            .filter(|path| path.starts_with(prefix) && remote.contains_key(*path))
            .count();
        if local.is_empty() && based_here > 0 {
            anyhow::bail!(
                "{} in the library folder is empty but held {based_here} files; nothing is removed from the store",
                prefix.trim_end_matches('/')
            );
        }
        let paths = remote
            .keys()
            .chain(local.keys())
            .cloned()
            .chain(known)
            .collect::<BTreeSet<_>>();
        for path in paths {
            let base = self.manifest.files.get(&path).cloned();
            let there = remote.get(&path);
            let here = match local.get(&path) {
                Some((size, modified)) => {
                    Some((self.local_sha(&path, *size, *modified)?, *size, *modified))
                }
                None => None,
            };
            let changed_here = here
                .as_ref()
                .is_some_and(|(sha, ..)| base.as_ref().is_none_or(|base| &base.local_sha != sha));
            let changed_there =
                there.is_some_and(|token| base.as_ref().is_none_or(|base| &base.remote != token));
            match (&here, there, &base) {
                (Some(_), Some(_), _) if !changed_here && !changed_there => {}
                (Some(_), Some(_), _) if changed_here && !changed_there => {
                    self.upload(&path)?;
                    report.uploaded.push(path.clone());
                }
                (Some(_), Some(_), _) if !changed_here && changed_there => {
                    self.download(&path)?;
                    report.downloaded.push(path.clone());
                }
                (Some((sha, ..)), Some(_), _) => {
                    // Both changed (or both new): the same bytes are no
                    // conflict.
                    let theirs = self.download_to_temporary(&path)?;
                    let their_sha = sha_of(&theirs)?;
                    if &their_sha == sha {
                        let _ = fs::remove_file(&theirs);
                    } else if path.starts_with("notes/") {
                        let copy = conflict_name(&path, &self.computer);
                        fs::rename(self.folder.join(&path), self.folder.join(&copy))?;
                        fs::rename(&theirs, self.folder.join(&path))?;
                        self.upload(&copy)?;
                        self.record(&copy)?;
                        report.conflicts.push(copy);
                        report.downloaded.push(path.clone());
                    } else {
                        // One writer, or named by uuid: the remote's is it.
                        fs::rename(&theirs, self.folder.join(&path))?;
                        report.downloaded.push(path.clone());
                    }
                }
                (Some(_), None, Some(_)) if !changed_here => {
                    // Removed there.
                    fs::remove_file(self.folder.join(&path))?;
                    self.manifest.files.remove(&path);
                    report.deleted_here.push(path.clone());
                    continue;
                }
                (Some(_), None, _) => {
                    self.upload(&path)?;
                    report.uploaded.push(path.clone());
                }
                (None, Some(_), Some(_)) if !changed_there => {
                    // Removed here.
                    self.runtime.block_on(self.operator.delete(&path))?;
                    self.manifest.files.remove(&path);
                    report.deleted_there.push(path.clone());
                    continue;
                }
                (None, Some(_), _) => {
                    self.download(&path)?;
                    report.downloaded.push(path.clone());
                }
                (None, None, _) => {
                    self.manifest.files.remove(&path);
                    continue;
                }
            }
            self.record(&path)?;
        }
        Ok(report)
    }

    /// Notes both sides' state of `path` as the base.
    fn record(&mut self, path: &str) -> Result<()> {
        let local = self.folder.join(path);
        let metadata = fs::metadata(&local)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |time| time.as_nanos() as u64);
        let stat = self.runtime.block_on(self.operator.stat(path))?;
        let remote = stat.etag().map(str::to_string).unwrap_or_else(|| {
            format!(
                "{}@{}",
                stat.content_length(),
                stat.last_modified()
                    .map(|time| time.to_string())
                    .unwrap_or_default()
            )
        });
        let local_sha = sha_of(&local)?;
        self.manifest.files.insert(
            path.to_string(),
            Base {
                remote,
                local_sha,
                local_size: metadata.len(),
                local_modified: modified,
            },
        );
        Ok(())
    }

    fn upload(&self, path: &str) -> Result<()> {
        let local = self.folder.join(path);
        self.runtime
            .block_on(async {
                let mut file = fs::File::open(&local)?;
                let mut writer = self.operator.writer_with(path).chunk(CHUNK).await?;
                let mut buffer = vec![0; CHUNK];
                loop {
                    let mut filled = 0;
                    while filled < buffer.len() {
                        let read = file.read(&mut buffer[filled..])?;
                        if read == 0 {
                            break;
                        }
                        filled += read;
                    }
                    if filled == 0 {
                        break;
                    }
                    writer.write(buffer[..filled].to_vec()).await?;
                    if filled < buffer.len() {
                        break;
                    }
                }
                writer.close().await?;
                anyhow::Ok(())
            })
            .with_context(|| format!("uploading {path}"))
    }

    /// The remote's `path` in a hidden file beside where it goes.
    fn download_to_temporary(&self, path: &str) -> Result<PathBuf> {
        let target = self.folder.join(path);
        let dir = target.parent().context("a file has a folder")?;
        fs::create_dir_all(dir)?;
        let name = target
            .file_name()
            .context("a file has a name")?
            .to_string_lossy();
        let temporary = dir.join(format!(".{name}.glyphst-download"));
        self.runtime
            .block_on(async {
                let size = self.operator.stat(path).await?.content_length();
                let reader = self.operator.reader(path).await?;
                let mut file = fs::File::create(&temporary)?;
                let mut at = 0;
                while at < size {
                    let end = (at + CHUNK as u64).min(size);
                    let part = reader.read(at..end).await?;
                    file.write_all(&part.to_vec())?;
                    at = end;
                }
                file.sync_all()?;
                anyhow::Ok(())
            })
            .with_context(|| format!("downloading {path}"))?;
        Ok(temporary)
    }

    fn download(&self, path: &str) -> Result<()> {
        let temporary = self.download_to_temporary(path)?;
        fs::rename(&temporary, self.folder.join(path))?;
        Ok(())
    }
}

fn sha_of(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// `notes/items/x.md` → `notes/items/x (conflict laptop 2026-10-20).md`.
fn conflict_name(path: &str, computer: &str) -> String {
    let date = {
        let seconds = crate::hlc::wall_ms() / 1000;
        let days = (seconds / 86_400) as i64;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        format!(
            "{}-{month:02}-{day:02}",
            yoe + era * 400 + i64::from(month <= 2)
        )
    };
    let (stem, extension) = match path.rsplit_once('.') {
        Some((stem, extension)) if !stem.ends_with('/') => (stem, format!(".{extension}")),
        _ => (path, String::new()),
    };
    format!("{stem} (conflict {computer} {date}){extension}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("glyphst-sync-mirror-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(root: &Path, path: &str) -> Option<String> {
        fs::read_to_string(root.join(path)).ok()
    }

    const ALL: [&str; 3] = ["sync/", "files/", "notes/"];

    fn computer(dir: &Path, name: &str, operator: &Operator) -> Mirror {
        let folder = dir.join(name);
        fs::create_dir_all(&folder).unwrap();
        Mirror::new(
            operator.clone(),
            &folder,
            &dir.join(format!("{name}-manifest.json")),
            name,
        )
        .unwrap()
    }

    #[test]
    fn two_computers_mirror_through_a_store() {
        let dir = folder("two");
        let operator = Operator::new(opendal::services::Memory::default()).unwrap();
        let mut a = computer(&dir, "a", &operator);
        let mut b = computer(&dir, "b", &operator);
        write(&a.folder, "sync/devices/1/device.json", "{}");
        write(
            &a.folder,
            "sync/devices/1/seg-000000000001.jsonl",
            "a's changes",
        );
        write(&a.folder, "files/k1/paper.pdf", "%PDF a big file");
        write(&a.folder, "notes/items/paper.md", "# Paper\n");
        // Stays on this computer.
        write(&a.folder, ".staging/x/half.pdf", "half");
        write(&a.folder, "notes/items/.paper.md.swp", "editor");
        let report = a.run(&ALL).unwrap();
        assert_eq!(report.uploaded.len(), 4, "{report:?}");

        let report = b.run(&ALL).unwrap();
        assert_eq!(report.downloaded.len(), 4, "{report:?}");
        assert_eq!(
            read(&b.folder, "files/k1/paper.pdf").as_deref(),
            Some("%PDF a big file")
        );
        assert_eq!(read(&b.folder, ".staging/x/half.pdf"), None);
        // Nothing changed: nothing moves.
        assert!(a.run(&ALL).unwrap().is_empty());
        assert!(b.run(&ALL).unwrap().is_empty());

        // b edits the note; a compacts its journal away.
        write(&b.folder, "notes/items/paper.md", "# Paper\nb's thoughts\n");
        b.run(&ALL).unwrap();
        fs::remove_file(a.folder.join("sync/devices/1/seg-000000000001.jsonl")).unwrap();
        let report = a.run(&ALL).unwrap();
        assert_eq!(report.downloaded, vec!["notes/items/paper.md".to_string()]);
        assert_eq!(
            report.deleted_there,
            vec!["sync/devices/1/seg-000000000001.jsonl".to_string()]
        );
        let report = b.run(&ALL).unwrap();
        assert_eq!(
            report.deleted_here,
            vec!["sync/devices/1/seg-000000000001.jsonl".to_string()]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_note_changed_on_both_keeps_both() {
        let dir = folder("conflict");
        let store = dir.join("store");
        fs::create_dir_all(&store).unwrap();
        let operator =
            Operator::new(opendal::services::Fs::default().root(&store.to_string_lossy())).unwrap();
        let mut a = computer(&dir, "a", &operator);
        let mut b = computer(&dir, "b", &operator);
        write(&a.folder, "notes/items/paper.md", "# Paper\n");
        a.run(&ALL).unwrap();
        b.run(&ALL).unwrap();

        write(&a.folder, "notes/items/paper.md", "# Paper\nfrom a\n");
        write(&b.folder, "notes/items/paper.md", "# Paper\nfrom b\n");
        a.run(&ALL).unwrap();
        let report = b.run(&ALL).unwrap();
        assert_eq!(report.conflicts.len(), 1, "{report:?}");
        let copy = &report.conflicts[0];
        assert!(copy.starts_with("notes/items/paper (conflict b "), "{copy}");
        assert_eq!(
            read(&b.folder, "notes/items/paper.md").as_deref(),
            Some("# Paper\nfrom a\n")
        );
        assert_eq!(read(&b.folder, copy).as_deref(), Some("# Paper\nfrom b\n"));
        // a gets b's copy too, and they agree from then on.
        a.run(&ALL).unwrap();
        assert_eq!(read(&a.folder, copy).as_deref(), Some("# Paper\nfrom b\n"));
        assert!(a.run(&ALL).unwrap().is_empty());
        assert!(b.run(&ALL).unwrap().is_empty());

        // The same edit on both is no conflict.
        write(&a.folder, "notes/items/paper.md", "# Same\n");
        write(&b.folder, "notes/items/paper.md", "# Same\n");
        a.run(&ALL).unwrap();
        assert!(b.run(&ALL).unwrap().conflicts.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_emptied_folder_does_not_empty_the_store() {
        let dir = folder("emptied");
        let operator = Operator::new(opendal::services::Memory::default()).unwrap();
        let mut a = computer(&dir, "a", &operator);
        write(&a.folder, "files/k1/paper.pdf", "%PDF");
        a.run(&ALL).unwrap();
        fs::remove_dir_all(a.folder.join("files")).unwrap();
        let error = a.run(&ALL).unwrap_err();
        assert!(
            format!("{error:#}").contains("nothing is removed"),
            "{error:#}"
        );
        let mut b = computer(&dir, "b", &operator);
        assert_eq!(b.run(&ALL).unwrap().downloaded.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn web_addresses_split_into_server_and_folder() {
        assert_eq!(
            split_url("https://cloud.example.org/remote.php/dav/files/me/Glyphst/"),
            (
                "https://cloud.example.org".to_string(),
                "/remote.php/dav/files/me/Glyphst".to_string()
            )
        );
        assert_eq!(
            split_url("http://nas:5005"),
            ("http://nas:5005".to_string(), "/".to_string())
        );
        assert_eq!(
            conflict_name("notes/a.b/x.md", "laptop")
                .rsplit_once(')')
                .unwrap()
                .1,
            ".md"
        );
    }
}

/// A small S3 for tests: path-style buckets, objects in memory, listing
/// two keys a page, ranges, single and batch deletes. Signatures are not
/// checked; the requests are opendal's own.
#[cfg(feature = "test-support")]
pub mod s3_stub {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
        thread,
    };

    use tiny_http::{Header, Method, Response, Server};

    type Objects = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

    pub struct Stub {
        pub endpoint: String,
        pub objects: Objects,
        server: Arc<Server>,
    }

    impl Drop for Stub {
        fn drop(&mut self) {
            self.server.unblock();
        }
    }

    fn decode(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%'
                && index + 2 < bytes.len()
                && let Ok(byte) = u8::from_str_radix(&text[index + 1..index + 3], 16)
            {
                out.push(byte);
                index += 3;
                continue;
            }
            out.push(if bytes[index] == b'+' {
                b' '
            } else {
                bytes[index]
            });
            index += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn etag(bytes: &[u8]) -> String {
        use sha2::{Digest as _, Sha256};
        format!("\"{}\"", &super::hex(&Sha256::digest(bytes))[..32])
    }

    fn header(name: &str, value: &str) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
    }

    pub fn start() -> Stub {
        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let endpoint = format!("http://{}", server.server_addr().to_ip().unwrap());
        let objects: Objects = Arc::default();
        let (serving, store) = (server.clone(), objects.clone());
        thread::spawn(move || {
            for mut request in serving.incoming_requests() {
                let url = request.url().to_string();
                let (path, query) = url.split_once('?').unwrap_or((&url, ""));
                let query = query
                    .split('&')
                    .filter(|pair| !pair.is_empty())
                    .map(|pair| {
                        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                        (key.to_string(), decode(value))
                    })
                    .collect::<BTreeMap<_, _>>();
                // `/<bucket>/<key>`.
                let key = path
                    .trim_start_matches('/')
                    .split_once('/')
                    .map(|(_, key)| decode(key))
                    .unwrap_or_default();
                let mut body = Vec::new();
                let _ = request.as_reader().read_to_end(&mut body);
                let mut objects = store.lock().unwrap();
                let response = match (request.method(), key.is_empty()) {
                    (Method::Get, true) if query.contains_key("list-type") => {
                        let prefix = query.get("prefix").cloned().unwrap_or_default();
                        let after = query.get("continuation-token").cloned().unwrap_or_default();
                        let keys = objects
                            .keys()
                            .filter(|key| key.starts_with(&prefix) && **key > after)
                            .cloned()
                            .collect::<Vec<_>>();
                        let page = keys.iter().take(2).collect::<Vec<_>>();
                        let more = keys.len() > 2;
                        let mut xml = String::from(
                            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult>",
                        );
                        xml.push_str(&format!("<IsTruncated>{more}</IsTruncated>"));
                        if more && let Some(last) = page.last() {
                            xml.push_str(&format!(
                                "<NextContinuationToken>{last}</NextContinuationToken>"
                            ));
                        }
                        for key in page {
                            let bytes = &objects[key];
                            xml.push_str(&format!(
                                "<Contents><Key>{key}</Key><Size>{}</Size><ETag>{}</ETag><LastModified>2026-10-20T00:00:00.000Z</LastModified></Contents>",
                                bytes.len(),
                                etag(bytes).replace('"', "&quot;")
                            ));
                        }
                        xml.push_str("</ListBucketResult>");
                        Response::from_data(xml.into_bytes())
                            .with_status_code(200)
                            .boxed()
                    }
                    (Method::Post, true) if query.contains_key("delete") => {
                        let text = String::from_utf8_lossy(&body).into_owned();
                        let mut xml = String::from("<DeleteResult>");
                        for part in text.split("<Key>").skip(1) {
                            let key = part.split("</Key>").next().unwrap_or_default();
                            objects.remove(key);
                            xml.push_str(&format!("<Deleted><Key>{key}</Key></Deleted>"));
                        }
                        xml.push_str("</DeleteResult>");
                        Response::from_data(xml.into_bytes())
                            .with_status_code(200)
                            .boxed()
                    }
                    (Method::Put, false) => {
                        let tag = etag(&body);
                        objects.insert(key, body);
                        Response::from_data(Vec::new())
                            .with_status_code(200)
                            .with_header(header("ETag", &tag))
                            .boxed()
                    }
                    (Method::Get | Method::Head, false) => match objects.get(&key) {
                        Some(bytes) => {
                            let range = request
                                .headers()
                                .iter()
                                .find(|h| h.field.equiv("Range"))
                                .and_then(|h| {
                                    let (start, end) =
                                        h.value.as_str().strip_prefix("bytes=")?.split_once('-')?;
                                    let start = start.parse::<usize>().ok()?;
                                    let end = end
                                        .parse::<usize>()
                                        .map_or(bytes.len(), |end| (end + 1).min(bytes.len()));
                                    Some((start, end))
                                });
                            let (start, end, status) = match range {
                                Some((start, end)) => (start, end, 206),
                                None => (0, bytes.len(), 200),
                            };
                            // A HEAD says the length and sends nothing.
                            let (data, length): (Box<dyn std::io::Read + Send>, usize) =
                                if request.method() == &Method::Head {
                                    (Box::new(std::io::empty()), bytes.len())
                                } else {
                                    (
                                        Box::new(std::io::Cursor::new(bytes[start..end].to_vec())),
                                        end - start,
                                    )
                                };
                            let mut response = Response::new(
                                tiny_http::StatusCode(status),
                                Vec::new(),
                                data,
                                Some(length),
                                None,
                            )
                            .with_chunked_threshold(usize::MAX)
                            .with_header(header("ETag", &etag(bytes)))
                            .with_header(header("Last-Modified", "Tue, 20 Oct 2026 00:00:00 GMT"));
                            if status == 206 {
                                response = response.with_header(header(
                                    "Content-Range",
                                    &format!("bytes {start}-{}/{}", end - 1, bytes.len()),
                                ));
                            }
                            response
                        }
                        None => {
                            Response::from_data(b"<Error><Code>NoSuchKey</Code></Error>".to_vec())
                                .with_status_code(404)
                                .boxed()
                        }
                    },
                    (Method::Delete, false) => {
                        objects.remove(&key);
                        Response::from_data(Vec::new())
                            .with_status_code(204)
                            .boxed()
                    }
                    _ => Response::from_data(Vec::new())
                        .with_status_code(400)
                        .boxed(),
                };
                drop(objects);
                let _ = request.respond(response);
            }
        });
        Stub {
            endpoint,
            objects,
            server,
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
mod s3_tests {
    use super::*;

    #[test]
    fn the_library_mirrors_through_s3() {
        let stub = s3_stub::start();
        let remote = Remote::S3 {
            endpoint: stub.endpoint.clone(),
            region: "auto".to_string(),
            bucket: "library".to_string(),
            prefix: "glyphst/".to_string(),
            access_key_id: "key".to_string(),
            secret_access_key: "secret".to_string(),
        };
        let dir = std::env::temp_dir().join(format!("glyphst-sync-s3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let operator = remote.operator().unwrap();
        let open = |name: &str| {
            let folder = dir.join(name);
            fs::create_dir_all(&folder).unwrap();
            Mirror::new(
                operator.clone(),
                &folder,
                &dir.join(format!("{name}.json")),
                name,
            )
            .unwrap()
        };
        let (mut a, mut b) = (open("a"), open("b"));
        assert_eq!(a.probe().unwrap(), 0);
        for index in 0..5 {
            let path = a
                .folder
                .join(format!("sync/devices/1/seg-00000000000{index}.jsonl"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("segment {index}")).unwrap();
        }
        let pdf = a.folder.join("files/k1/paper.pdf");
        fs::create_dir_all(pdf.parent().unwrap()).unwrap();
        fs::write(&pdf, vec![7u8; 100_000]).unwrap();
        a.run(&["sync/", "files/"]).unwrap();
        assert!(
            stub.objects
                .lock()
                .unwrap()
                .contains_key("glyphst/files/k1/paper.pdf")
        );
        // Listed two to a page; all arrive.
        let report = b.run(&["sync/", "files/"]).unwrap();
        assert_eq!(report.downloaded.len(), 6, "{report:?}");
        assert_eq!(
            fs::read(b.folder.join("files/k1/paper.pdf")).unwrap().len(),
            100_000
        );
        assert!(b.run(&["sync/", "files/"]).unwrap().is_empty());
        assert_eq!(b.probe().unwrap(), 5);

        fs::remove_file(a.folder.join("sync/devices/1/seg-000000000000.jsonl")).unwrap();
        a.run(&["sync/"]).unwrap();
        assert_eq!(b.run(&["sync/"]).unwrap().deleted_here.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Against a real bucket (R2, B2, MinIO), when its settings are in
    /// the environment: `GLYPHST_SYNC_S3_ENDPOINT`, `_REGION`, `_BUCKET`,
    /// `_KEY_ID`, `_SECRET`. Writes into a fresh prefix and removes it.
    #[test]
    #[ignore = "needs a real bucket"]
    fn a_real_bucket_takes_the_library() {
        let var = |name: &str| std::env::var(format!("GLYPHST_SYNC_S3_{name}")).unwrap();
        let prefix = format!("glyphst-test-{}/", uuid::Uuid::new_v4());
        let remote = Remote::S3 {
            endpoint: var("ENDPOINT"),
            region: std::env::var("GLYPHST_SYNC_S3_REGION").unwrap_or_default(),
            bucket: var("BUCKET"),
            prefix: prefix.clone(),
            access_key_id: var("KEY_ID"),
            secret_access_key: var("SECRET"),
        };
        let dir = std::env::temp_dir().join(format!("glyphst-sync-real-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let folder = dir.join("a");
        let pdf = folder.join("files/k1/paper.pdf");
        fs::create_dir_all(pdf.parent().unwrap()).unwrap();
        // Over a chunk, so it goes up in parts.
        fs::write(&pdf, vec![3u8; CHUNK + 1_000]).unwrap();
        let operator = remote.operator().unwrap();
        let mut mirror = Mirror::new(operator.clone(), &folder, &dir.join("a.json"), "a").unwrap();
        assert_eq!(mirror.probe().unwrap(), 0);
        assert_eq!(mirror.run(&["files/"]).unwrap().uploaded.len(), 1);
        let other = dir.join("b");
        fs::create_dir_all(&other).unwrap();
        let mut second = Mirror::new(operator.clone(), &other, &dir.join("b.json"), "b").unwrap();
        assert_eq!(second.run(&["files/"]).unwrap().downloaded.len(), 1);
        assert_eq!(
            fs::read(other.join("files/k1/paper.pdf")).unwrap(),
            fs::read(&pdf).unwrap()
        );
        fs::remove_file(&pdf).unwrap();
        mirror.run(&["files/"]).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
