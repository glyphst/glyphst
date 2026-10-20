//! The journal: each machine's changes as files in the library folder,
//! under `sync/devices/<device>/`, written by that machine only.
//!
//! - `device.json`: its name and when it joined.
//! - `seg-<seq>.jsonl`: changes, numbered from 1, never changed once
//!   written. A header line, a line per change, and a trailer with the
//!   count and the SHA-256 of what came before it: a file without a valid
//!   trailer is still arriving.
//! - `snapshot-<seq>.jsonl`: the machine's whole merged state through its
//!   segment `seq`, with the segments of others it had read (`covers`).
//!   After writing one a machine removes its segments up to it; a machine
//!   that fell behind reads the snapshot instead.
//!
//! Other names in a device's folder (a sync tool's conflict copies) are
//! strays: in this machine's own folder they mean another machine writes
//! there too.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{hlc::DeviceId, model::Change};

/// The journal's folder in the library folder.
pub const SYNC_FOLDER: &str = "sync";
const DEVICES: &str = "devices";
const DEVICE_FILE: &str = "device.json";
const FORMAT: u32 = 1;
/// A segment is cut at about this size.
pub const SEGMENT_BYTES: usize = 4 * 1024 * 1024;
/// A machine compacts its journal past this many segments, or bytes.
pub const COMPACT_SEGMENTS: usize = 200;
pub const COMPACT_BYTES: u64 = 8 * 1024 * 1024;
/// Snapshots a machine keeps of its own.
const KEEP_SNAPSHOTS: usize = 2;

/// Segments read through, per device.
pub type Watermarks = BTreeMap<DeviceId, u64>;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    #[serde(default)]
    pub name: String,
    /// Milliseconds since the epoch.
    #[serde(default)]
    pub created: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FileKind {
    Segment,
    Snapshot,
}

#[derive(Serialize, Deserialize)]
struct Header {
    glyphst_sync: u32,
    file: FileKind,
    device: DeviceId,
    seq: u64,
    /// When it was written, milliseconds since the epoch.
    at: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    covers: Watermarks,
}

#[derive(Serialize, Deserialize)]
struct Trailer {
    end: usize,
    sha256: String,
}

/// A segment or snapshot as written: what an outbox keeps until the file is.
pub fn encode_segment(device: DeviceId, seq: u64, at: u64, changes: &[Change]) -> Vec<u8> {
    encode(
        FileKind::Segment,
        device,
        seq,
        at,
        &Watermarks::new(),
        changes,
    )
}

fn encode(
    file: FileKind,
    device: DeviceId,
    seq: u64,
    at: u64,
    covers: &Watermarks,
    changes: &[Change],
) -> Vec<u8> {
    let header = Header {
        glyphst_sync: FORMAT,
        file,
        device,
        seq,
        at,
        covers: covers.clone(),
    };
    let mut bytes = serde_json::to_vec(&header).expect("a header serializes");
    bytes.push(b'\n');
    for change in changes {
        serde_json::to_writer(&mut bytes, change).expect("a change serializes");
        bytes.push(b'\n');
    }
    let trailer = Trailer {
        end: changes.len(),
        sha256: hex(&Sha256::digest(&bytes)),
    };
    serde_json::to_writer(&mut bytes, &trailer).expect("a trailer serializes");
    bytes.push(b'\n');
    bytes
}

/// The file's header and changes, or `None` while it is incomplete.
fn decode(bytes: &[u8]) -> Option<(Header, Vec<Change>)> {
    let body = bytes.strip_suffix(b"\n")?;
    let split = body.iter().rposition(|byte| *byte == b'\n')? + 1;
    let trailer: Trailer = serde_json::from_slice(&body[split..]).ok()?;
    if trailer.sha256 != hex(&Sha256::digest(&body[..split])) {
        return None;
    }
    let mut lines = body[..split - 1].split(|byte| *byte == b'\n');
    let header: Header = serde_json::from_slice(lines.next()?).ok()?;
    if header.glyphst_sync != FORMAT {
        return None;
    }
    let changes = lines
        .map(serde_json::from_slice)
        .collect::<Result<Vec<Change>, _>>()
        .ok()?;
    (changes.len() == trailer.end).then_some((header, changes))
}

/// The digest a complete file's trailer names.
fn digest(bytes: &[u8]) -> Option<String> {
    let body = bytes.strip_suffix(b"\n")?;
    let split = body.iter().rposition(|byte| *byte == b'\n')? + 1;
    serde_json::from_slice::<Trailer>(&body[split..])
        .ok()
        .map(|trailer| trailer.sha256)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Changes cut into segments of about [`SEGMENT_BYTES`].
pub fn split_changes(changes: Vec<Change>) -> Vec<Vec<Change>> {
    let mut segments = Vec::new();
    let mut current = Vec::new();
    let mut size = 0;
    for change in changes {
        let length = serde_json::to_vec(&change).map_or(0, |bytes| bytes.len() + 1);
        if size + length > SEGMENT_BYTES && !current.is_empty() {
            segments.push(std::mem::take(&mut current));
            size = 0;
        }
        size += length;
        current.push(change);
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// One device's folder as listed.
#[derive(Clone, Debug, Default)]
pub struct DeviceListing {
    pub id: DeviceId,
    pub info: Option<DeviceInfo>,
    /// Segment numbers and their sizes.
    pub segments: BTreeMap<u64, u64>,
    pub snapshots: BTreeSet<u64>,
    /// Names that are not the journal's.
    pub strays: Vec<String>,
    /// When its newest segment or snapshot was written here.
    pub last_written: Option<SystemTime>,
}

impl DeviceListing {
    pub fn should_compact(&self) -> bool {
        self.segments.len() > COMPACT_SEGMENTS
            || self.segments.values().sum::<u64>() > COMPACT_BYTES
    }

    /// Whether this machine's own folder shows another writer: a segment
    /// past the last one it wrote (`next_seq` is the one it writes next), a
    /// sync tool's conflict copy, or a segment that is not what it wrote
    /// (`written` holds the digests it recorded). That happens when a copy
    /// of the library database runs on a second machine.
    pub fn forked(
        &self,
        next_seq: u64,
        written: &BTreeMap<u64, String>,
        journal: &Journal,
    ) -> bool {
        !self.strays.is_empty()
            || self.segments.range(next_seq..).next().is_some()
            || written.iter().any(|(seq, digest)| {
                self.segments.contains_key(seq)
                    && journal
                        .segment_digest(self.id, *seq)
                        .is_some_and(|found| &found != digest)
            })
    }
}

/// Changes read from one file of another machine.
#[derive(Clone, Debug)]
pub struct Batch {
    pub device: DeviceId,
    pub seq: u64,
    /// A snapshot's segments of others it had read.
    pub covers: Option<Watermarks>,
    pub changes: Vec<Change>,
}

impl Batch {
    /// Marks the batch read: its device through `seq`, and for a
    /// snapshot, the others it covers too.
    pub fn advance(&self, marks: &mut Watermarks, own: DeviceId) {
        raise(marks, self.device, self.seq);
        for (device, seq) in self.covers.iter().flatten() {
            if *device != own {
                raise(marks, *device, *seq);
            }
        }
    }
}

fn raise(marks: &mut Watermarks, device: DeviceId, seq: u64) {
    let mark = marks.entry(device).or_default();
    *mark = (*mark).max(seq);
}

/// The `sync/` folder of a library folder.
#[derive(Clone, Debug)]
pub struct Journal {
    root: PathBuf,
}

impl Journal {
    pub fn in_library(library_folder: &Path) -> Self {
        Self {
            root: library_folder.join(SYNC_FOLDER),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn device_dir(&self, device: DeviceId) -> PathBuf {
        self.root.join(DEVICES).join(device.to_string())
    }

    fn segment_path(&self, device: DeviceId, seq: u64) -> PathBuf {
        self.device_dir(device).join(format!("seg-{seq:012}.jsonl"))
    }

    fn snapshot_path(&self, device: DeviceId, seq: u64) -> PathBuf {
        self.device_dir(device)
            .join(format!("snapshot-{seq:012}.jsonl"))
    }

    pub fn write_device(&self, device: DeviceId, info: &DeviceInfo) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(info)?;
        write_atomic(&self.device_dir(device).join(DEVICE_FILE), &bytes)
    }

    /// Writes a segment encoded by [`encode_segment`].
    pub fn write_segment(&self, device: DeviceId, seq: u64, bytes: &[u8]) -> Result<()> {
        write_atomic(&self.segment_path(device, seq), bytes)
    }

    /// The trailer digest of this machine's segment `seq` as it is on
    /// disk, if it is complete.
    pub fn segment_digest(&self, device: DeviceId, seq: u64) -> Option<String> {
        fs::read(self.segment_path(device, seq))
            .ok()
            .and_then(|bytes| digest(&bytes))
    }

    /// The digest of an encoded segment.
    pub fn digest_of(bytes: &[u8]) -> Option<String> {
        digest(bytes)
    }

    /// Every device that has a folder, this one's included.
    pub fn devices(&self) -> Result<Vec<DeviceListing>> {
        let dir = self.root.join(DEVICES);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
        };
        let mut devices = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<DeviceId>().ok())
            else {
                continue;
            };
            if entry.file_type()?.is_dir() {
                devices.push(self.list_device(id)?);
            }
        }
        devices.sort_by_key(|device| device.id);
        Ok(devices)
    }

    pub fn list_device(&self, id: DeviceId) -> Result<DeviceListing> {
        let dir = self.device_dir(id);
        let mut listing = DeviceListing {
            id,
            ..DeviceListing::default()
        };
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(listing),
            Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let metadata = entry.metadata()?;
            let numbered = |prefix: &str| {
                name.strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix(".jsonl"))
                    .filter(|digits| digits.len() == 12)
                    .and_then(|digits| digits.parse::<u64>().ok())
            };
            if name == DEVICE_FILE {
                listing.info = fs::read(entry.path())
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                continue;
            }
            if let Some(seq) = numbered("seg-") {
                listing.segments.insert(seq, metadata.len());
            } else if let Some(seq) = numbered("snapshot-") {
                listing.snapshots.insert(seq);
            } else {
                // Files being written, and a cloud drive's placeholders
                // (`.seg-….jsonl.icloud`), are hidden; anything else is
                // somebody else's.
                if !name.starts_with('.') {
                    listing.strays.push(name);
                }
                continue;
            }
            if let Ok(modified) = metadata.modified() {
                listing.last_written = listing.last_written.max(Some(modified));
            }
        }
        listing.strays.sort();
        Ok(listing)
    }

    fn read(&self, path: &Path, kind: FileKind, device: DeviceId, seq: u64) -> Option<Batch> {
        let bytes = fs::read(path).ok()?;
        let (header, changes) = decode(&bytes)?;
        if header.file != kind || header.device != device || header.seq != seq {
            return None;
        }
        Some(Batch {
            device,
            seq,
            covers: (kind == FileKind::Snapshot).then_some(header.covers),
            changes,
        })
    }

    /// Other machines' changes not yet read, in each machine's order, about
    /// `max_changes` at most (always at least one file when one is ready).
    pub fn pending(
        &self,
        own: DeviceId,
        marks: &Watermarks,
        max_changes: usize,
    ) -> Result<Vec<Batch>> {
        let mut batches = Vec::new();
        let mut count = 0;
        for device in self.devices()? {
            if device.id == own {
                continue;
            }
            let mut next = marks.get(&device.id).copied().unwrap_or(0) + 1;
            while count < max_changes || batches.is_empty() {
                let batch = if device.segments.contains_key(&next) {
                    self.read(
                        &self.segment_path(device.id, next),
                        FileKind::Segment,
                        device.id,
                        next,
                    )
                } else if let Some(&seq) = device.snapshots.range(next..).next_back() {
                    // The segments are gone: compacted into a snapshot.
                    self.read(
                        &self.snapshot_path(device.id, seq),
                        FileKind::Snapshot,
                        device.id,
                        seq,
                    )
                } else {
                    None
                };
                // Not arrived yet (or not written): wait for it.
                let Some(batch) = batch else { break };
                next = batch.seq + 1;
                count += batch.changes.len();
                batches.push(batch);
            }
        }
        Ok(batches)
    }

    /// Writes this machine's snapshot through its segment `seq` and
    /// removes what it replaces: the segments up to it, and all but the
    /// newest snapshots.
    pub fn compact(
        &self,
        own: DeviceId,
        seq: u64,
        at: u64,
        covers: &Watermarks,
        changes: &[Change],
    ) -> Result<()> {
        let bytes = encode(FileKind::Snapshot, own, seq, at, covers, changes);
        write_atomic(&self.snapshot_path(own, seq), &bytes)?;
        let listing = self.list_device(own)?;
        for segment in listing.segments.keys().filter(|segment| **segment <= seq) {
            remove_if_present(&self.segment_path(own, *segment))?;
        }
        let keep = listing
            .snapshots
            .iter()
            .rev()
            .take(KEEP_SNAPSHOTS)
            .copied()
            .collect::<BTreeSet<_>>();
        for snapshot in listing.snapshots.difference(&keep) {
            remove_if_present(&self.snapshot_path(own, *snapshot))?;
        }
        Ok(())
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Writes `bytes` beside `path` under a hidden name, then renames it into
/// place: a sync tool never sees half a file under the real name.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("a journal file has a folder")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path
        .file_name()
        .context("a journal file has a name")?
        .to_string_lossy();
    let temporary = dir.join(format!(".{name}.writing-{}", std::process::id()));
    let mut file =
        fs::File::create(&temporary).with_context(|| format!("writing {}", temporary.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        hlc::Clock,
        model::{RecordKey, State},
    };

    fn folder(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "glyphst-sync-journal-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    fn change(clock: &mut Clock, wall: u64, key: &str, title: &str) -> Change {
        Change {
            hlc: clock.now(wall),
            kind: "item".to_string(),
            key: key.to_string(),
            fields: [("title".to_string(), json!(title))].into(),
        }
    }

    fn apply(batches: &[Batch], state: &mut State, marks: &mut Watermarks, own: DeviceId) {
        for batch in batches {
            for change in &batch.changes {
                state.apply(change);
            }
            batch.advance(marks, own);
        }
    }

    #[test]
    fn a_segment_is_read_once_it_is_complete() {
        let root = folder("partial");
        let journal = Journal::in_library(&root);
        let (writer, reader) = (DeviceId::new(), DeviceId::new());
        let mut clock = Clock::new(writer, None);
        let bytes = encode_segment(writer, 1, 5, &[change(&mut clock, 10, "a", "one")]);

        // A sync tool has delivered half of it.
        journal
            .write_segment(writer, 1, &bytes[..bytes.len() - 9])
            .unwrap();
        assert!(
            journal
                .pending(reader, &Watermarks::new(), 100)
                .unwrap()
                .is_empty()
        );

        journal.write_segment(writer, 1, &bytes).unwrap();
        let batches = journal.pending(reader, &Watermarks::new(), 100).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].changes[0].fields["title"], json!("one"));

        // Read through it, there is nothing more.
        let mut marks = Watermarks::new();
        batches[0].advance(&mut marks, reader);
        assert!(journal.pending(reader, &marks, 100).unwrap().is_empty());
        assert_eq!(
            journal.segment_digest(writer, 1),
            Journal::digest_of(&bytes)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_reader_behind_a_compaction_reads_the_snapshot() {
        let root = folder("compact");
        let journal = Journal::in_library(&root);
        let (writer, other, early, late) = (
            DeviceId::new(),
            DeviceId::new(),
            DeviceId::new(),
            DeviceId::new(),
        );
        let mut clock = Clock::new(writer, None);
        let mut other_clock = Clock::new(other, None);

        // Another machine's segment, read by the writer.
        let from_other = vec![change(&mut other_clock, 5, "b", "theirs")];
        journal
            .write_segment(other, 1, &encode_segment(other, 1, 0, &from_other))
            .unwrap();

        let mut writer_state = State::default();
        for change in &from_other {
            writer_state.apply(change);
        }
        let mut all = Vec::new();
        for seq in 1..=5u64 {
            let changes = vec![
                change(&mut clock, seq * 10, "a", &format!("v{seq}")),
                change(&mut clock, seq * 10, &format!("k{seq}"), "x"),
            ];
            for change in &changes {
                writer_state.apply(change);
            }
            journal
                .write_segment(writer, seq, &encode_segment(writer, seq, 0, &changes))
                .unwrap();
            all.extend(changes);
        }

        // One reader keeps up through segment 2 ...
        let mut early_state = State::default();
        let mut early_marks = Watermarks::new();
        let mut first = journal.pending(early, &early_marks, 1_000).unwrap();
        first.retain(|batch| batch.device != writer || batch.seq <= 2);
        apply(&first, &mut early_state, &mut early_marks, early);
        assert_eq!(early_marks.get(&writer), Some(&2));

        // ... then the writer compacts through 4 and writes one more.
        let covers = Watermarks::from([(other, 1)]);
        journal
            .compact(writer, 4, 0, &covers, &writer_state.changes())
            .unwrap();
        let listing = journal.list_device(writer).unwrap();
        assert_eq!(
            listing.segments.keys().copied().collect::<Vec<_>>(),
            vec![5]
        );
        assert_eq!(listing.snapshots, BTreeSet::from([4]));

        let rest = journal.pending(early, &early_marks, 1_000).unwrap();
        apply(&rest, &mut early_state, &mut early_marks, early);

        // A machine that joins now reads the snapshot and segment 5, and
        // need not read the other machine's segment the snapshot covers.
        let mut late_state = State::default();
        let mut late_marks = Watermarks::new();
        let batches = journal.pending(late, &late_marks, 1_000).unwrap();
        apply(&batches, &mut late_state, &mut late_marks, late);
        assert_eq!(late_marks.get(&other), Some(&1));
        assert!(
            journal
                .pending(late, &late_marks, 1_000)
                .unwrap()
                .is_empty()
        );

        assert_eq!(early_state, writer_state);
        assert_eq!(late_state, writer_state);
        assert_eq!(
            late_state.record(&RecordKey::new("item", "a")).unwrap()["title"].value,
            json!("v5")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn strays_in_a_device_folder_are_listed() {
        let root = folder("strays");
        let journal = Journal::in_library(&root);
        let device = DeviceId::new();
        journal
            .write_device(
                device,
                &DeviceInfo {
                    name: "desk".to_string(),
                    created: 1,
                },
            )
            .unwrap();
        journal
            .write_segment(device, 1, &encode_segment(device, 1, 0, &[]))
            .unwrap();
        let dir = root.join("sync/devices").join(device.to_string());
        fs::write(
            dir.join("seg-000000000001.sync-conflict-20261020-1.jsonl"),
            b"x",
        )
        .unwrap();
        fs::write(dir.join(".seg-000000000002.jsonl.icloud"), b"").unwrap();

        let listing = journal.list_device(device).unwrap();
        assert_eq!(listing.info.unwrap().name, "desk");
        assert_eq!(listing.segments.len(), 1);
        assert_eq!(
            listing.strays,
            vec!["seg-000000000001.sync-conflict-20261020-1.jsonl".to_string()]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_second_writer_in_this_machines_folder_is_noticed() {
        let root = folder("fork");
        let journal = Journal::in_library(&root);
        let device = DeviceId::new();
        let mut clock = Clock::new(device, None);
        let ours = encode_segment(device, 1, 0, &[change(&mut clock, 1, "a", "ours")]);
        journal.write_segment(device, 1, &ours).unwrap();
        let written = BTreeMap::from([(1, Journal::digest_of(&ours).unwrap())]);
        let listing = journal.list_device(device).unwrap();
        assert!(!listing.forked(2, &written, &journal));

        // A copy of this library elsewhere wrote segment 2 ...
        let mut copy = Clock::new(device, None);
        let theirs = encode_segment(device, 2, 0, &[change(&mut copy, 1, "b", "copy")]);
        journal.write_segment(device, 2, &theirs).unwrap();
        assert!(
            journal
                .list_device(device)
                .unwrap()
                .forked(2, &written, &journal)
        );

        // ... or wrote its own segment 1 over ours.
        let other = encode_segment(device, 1, 0, &[change(&mut copy, 2, "c", "copy")]);
        journal.write_segment(device, 1, &other).unwrap();
        fs::remove_file(root.join(format!("sync/devices/{device}/seg-000000000002.jsonl")))
            .unwrap();
        assert!(
            journal
                .list_device(device)
                .unwrap()
                .forked(2, &written, &journal)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn large_exports_are_cut_into_segments() {
        let mut clock = Clock::new(DeviceId::new(), None);
        let long = "x".repeat(100_000);
        let changes = (0..100)
            .map(|index| change(&mut clock, 1, &index.to_string(), &long))
            .collect::<Vec<_>>();
        let segments = split_changes(changes);
        assert!(segments.len() >= 3);
        assert_eq!(segments.iter().map(Vec::len).sum::<usize>(), 100);
    }
}
