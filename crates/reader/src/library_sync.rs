//! Syncing the library with this person's other machines through the
//! library folder (a sync tool carries it): a worker thread works out what
//! changed here and writes it into this machine's journal, reads the other
//! machines' journals, and keeps the journal compact; the UI thread takes
//! the changes into the library a few at a time. Nothing is applied while
//! something here is not yet exported, so a newer local edit is never
//! overwritten by an older one from elsewhere.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant, SystemTime},
};

use glyphst_library::{LibraryStore, SyncExport, SyncReader, SyncSnapshot};
use glyphst_sync::{Batch, Change, DeviceId, DeviceInfo, DeviceListing, Journal, Watermarks};

/// How long after the last change here an export waits.
const EXPORT_AFTER: Duration = Duration::from_secs(2);
/// How often the other machines' journals are read.
const READ_EVERY: Duration = Duration::from_secs(5);
/// Changes taken into the library per frame.
const APPLY_PER_FRAME: usize = 500;
/// Conflicts kept for Settings.
const CONFLICTS_KEPT: usize = 50;

enum Work {
    Diff {
        snapshot: Box<SyncSnapshot>,
        reader: SyncReader,
    },
    Write {
        journal: Journal,
        device: DeviceId,
        info: DeviceInfo,
        segments: Vec<(u64, Vec<u8>)>,
    },
    Read {
        journal: Journal,
        own: DeviceId,
        marks: Watermarks,
        next_seq: u64,
        written: BTreeMap<u64, String>,
    },
    Compact {
        journal: Journal,
        own: DeviceId,
        seq: u64,
        covers: Watermarks,
        changes: Vec<Change>,
    },
}

enum Done {
    Diffed(Box<SyncExport>),
    Written(Result<Vec<(u64, String)>, String>),
    Read(Result<Read, String>),
    Compacted(Result<(), String>),
}

struct Read {
    batches: Vec<Batch>,
    devices: Vec<DeviceListing>,
    forked: bool,
    compact: bool,
}

/// Another machine, as Settings shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceStatus {
    pub id: DeviceId,
    pub name: String,
    pub this: bool,
    pub last_written: Option<SystemTime>,
    /// Its clock runs ahead of this one's by this much.
    pub clock_ahead: Option<Duration>,
}

/// How the sync stands, for Settings and the status bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncStatus {
    pub devices: Vec<DeviceStatus>,
    /// When this machine last wrote or took in changes.
    pub last_synced: Option<SystemTime>,
    pub error: Option<String>,
    /// Records waiting for what they refer to.
    pub parked: usize,
    /// What could not be kept, newest last.
    pub conflicts: Vec<String>,
    /// Copied files other machines added that have not arrived.
    pub files_waiting: usize,
}

pub struct LibrarySync {
    requests: Sender<Work>,
    results: Receiver<Done>,
    /// The library folder synced, `None` while off.
    folder: Option<PathBuf>,
    /// The sync revision the last export saw, `None` to export again.
    exported: Option<u64>,
    /// When the sync revision last moved, to wait for edits to settle.
    revision_seen: Option<(u64, Instant)>,
    diffing: Option<u64>,
    writing: bool,
    reading: bool,
    compacting: bool,
    last_read: Option<Instant>,
    /// Batches read and not yet taken in.
    pending: Vec<Batch>,
    expected: BTreeSet<PathBuf>,
    arrived: u64,
    clock_ahead: BTreeMap<DeviceId, u64>,
    status: SyncStatus,
    export_after: Duration,
    read_every: Duration,
}

impl LibrarySync {
    pub fn new(ctx: &egui::Context) -> Self {
        let (requests, wanted) = channel::<Work>();
        let (finished, results) = channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("library-sync".into())
            .spawn(move || {
                for work in wanted {
                    if finished.send(run(work)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(error) = spawned {
            log::warn!("no library sync thread: {error}");
        }
        Self {
            requests,
            results,
            folder: None,
            exported: None,
            revision_seen: None,
            diffing: None,
            writing: false,
            reading: false,
            compacting: false,
            last_read: None,
            pending: Vec::new(),
            expected: BTreeSet::new(),
            arrived: 0,
            clock_ahead: BTreeMap::new(),
            status: SyncStatus::default(),
            export_after: EXPORT_AFTER,
            read_every: READ_EVERY,
        }
    }

    pub fn status(&self) -> &SyncStatus {
        &self.status
    }

    /// Counts copied files from other machines as they arrive, so search
    /// reads them.
    pub fn files_arrived(&self) -> u64 {
        self.arrived
    }

    /// Whether changes from other machines are still being taken in.
    #[cfg(test)]
    fn inbound(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether nothing here waits to be exported and nothing from other
    /// computers waits to be taken in (the vault writes notes only then,
    /// so a note shows a state every computer will have). Always while
    /// the library does not sync.
    pub fn quiet(&self, library: &LibraryStore) -> bool {
        self.folder.is_none()
            || (self.pending.is_empty()
                && self.diffing.is_none()
                && self.exported == Some(library.sync_revision()))
    }

    /// Whether the thread is at work (screenshots wait).
    pub fn busy(&self) -> bool {
        self.diffing.is_some() || self.writing || self.reading || self.compacting
    }

    /// Shows `status` (the screenshots' demo, which never syncs).
    pub fn show_status(&mut self, status: SyncStatus) {
        self.status = status;
    }

    /// Reads the other machines now.
    pub fn sync_now(&mut self) {
        self.last_read = None;
        self.exported = None;
    }

    #[cfg(test)]
    fn sync_now_for_tests(&mut self) {
        self.last_read = None;
        self.exported = None;
    }

    /// Takes in what the thread finished and starts what is due. `folder`
    /// is the library folder synced, `None` while sync is off.
    pub fn poll(&mut self, library: &mut LibraryStore, folder: Option<PathBuf>) -> Vec<String> {
        let mut notices = Vec::new();
        if folder != self.folder {
            self.folder = folder;
            self.exported = None;
            self.last_read = None;
            self.pending.clear();
            self.status = SyncStatus::default();
        }
        while let Ok(done) = self.results.try_recv() {
            self.take(done, library, &mut notices);
        }
        let Some(folder) = self.folder.clone() else {
            return notices;
        };
        let journal = Journal::in_library(&folder);
        let now = Instant::now();

        // Export once edits here settle.
        let revision = library.sync_revision();
        if self.revision_seen.is_none_or(|(seen, _)| seen != revision) {
            self.revision_seen = Some((revision, now));
        }
        let settled = self
            .revision_seen
            .is_some_and(|(_, at)| now.duration_since(at) >= self.export_after)
            || self.exported.is_none();
        if self.exported != Some(revision)
            && settled
            && self.diffing.is_none()
            && let Some(reader) = library.sync_reader()
        {
            match library.sync_snapshot() {
                Ok(snapshot) => {
                    let work = Work::Diff {
                        snapshot: Box::new(snapshot),
                        reader,
                    };
                    if self.requests.send(work).is_ok() {
                        self.diffing = Some(revision);
                    }
                }
                Err(error) => self.status.error = Some(format!("{error:#}")),
            }
        }

        // Write what the outbox holds (after a crash too).
        if !self.writing {
            match library.sync_outbox() {
                Ok(segments) if !segments.is_empty() => {
                    let work = Work::Write {
                        journal: journal.clone(),
                        device: library.sync_device(),
                        info: DeviceInfo {
                            name: device_name(),
                            created: glyphst_sync::wall_ms(),
                        },
                        segments,
                    };
                    self.writing = self.requests.send(work).is_ok();
                }
                Ok(_) => {}
                Err(error) => self.status.error = Some(format!("{error:#}")),
            }
        }

        // Read the others.
        if !self.reading
            && self.pending.is_empty()
            && self
                .last_read
                .is_none_or(|last| now.duration_since(last) >= self.read_every)
        {
            self.last_read = Some(now);
            let work = Work::Read {
                journal: journal.clone(),
                own: library.sync_device(),
                marks: library.sync_watermarks().clone(),
                next_seq: library.sync_next_seq(),
                written: library.sync_written().clone(),
            };
            self.reading = self.requests.send(work).is_ok();
            // Files other machines added may have arrived since.
            let before = self.expected.len();
            self.expected.retain(|path| !path.exists());
            if self.expected.len() != before {
                self.arrived += 1;
            }
            self.status.files_waiting = self.expected.len();
        }

        // Take changes in, once nothing here is waiting to be exported.
        let quiet = self.diffing.is_none() && self.exported == Some(library.sync_revision());
        if quiet && !self.pending.is_empty() {
            let chunk = take_chunk(&mut self.pending, APPLY_PER_FRAME);
            match library.apply_sync(&chunk) {
                Ok(report) => {
                    if report.fields > 0 {
                        self.status.last_synced = Some(SystemTime::now());
                    }
                    self.status.parked = report.parked;
                    for conflict in report.conflicts {
                        notices.push(conflict.clone());
                        self.status.conflicts.push(conflict);
                    }
                    let excess = self.status.conflicts.len().saturating_sub(CONFLICTS_KEPT);
                    self.status.conflicts.drain(..excess);
                    self.expected.extend(report.files_expected);
                    self.status.files_waiting = self.expected.len();
                    for (device, ahead) in report.clock_ahead {
                        self.clock_ahead.insert(device, ahead);
                    }
                    self.refresh_clocks();
                }
                Err(error) => {
                    // Read again from the last batch taken in.
                    self.pending.clear();
                    self.status.error = Some(format!("Could not take in changes: {error:#}"));
                }
            }
            // What applying did here (merges, removals) is exported next.
            self.exported = None;
        }
        notices
    }

    fn take(&mut self, done: Done, library: &mut LibraryStore, notices: &mut Vec<String>) {
        match done {
            Done::Diffed(export) => {
                let revision = self.diffing.take();
                match library.commit_export(*export) {
                    Ok(_) => {
                        self.exported = revision;
                    }
                    Err(error) => {
                        self.status.error = Some(format!("Could not export: {error:#}"));
                    }
                }
            }
            Done::Written(result) => {
                self.writing = false;
                match result {
                    Ok(written) => {
                        for (seq, digest) in written {
                            if let Err(error) = library.sync_segment_written(seq, digest) {
                                self.status.error = Some(format!("{error:#}"));
                            }
                        }
                        self.status.last_synced = Some(SystemTime::now());
                        self.status.error = None;
                    }
                    Err(error) => self.status.error = Some(error),
                }
            }
            Done::Read(result) => {
                self.reading = false;
                match result {
                    Ok(read) => {
                        if read.forked {
                            notices.push(
                                "Another copy of this library writes as this computer; it now syncs as a new one."
                                    .to_string(),
                            );
                            if let Err(error) = library.sync_fork() {
                                self.status.error = Some(format!("{error:#}"));
                            }
                            return;
                        }
                        self.pending = read.batches;
                        self.status.devices = read
                            .devices
                            .iter()
                            .map(|device| DeviceStatus {
                                id: device.id,
                                name: device
                                    .info
                                    .as_ref()
                                    .map(|info| info.name.clone())
                                    .filter(|name| !name.is_empty())
                                    .unwrap_or_else(|| "another computer".to_string()),
                                this: device.id == library.sync_device(),
                                last_written: device.last_written,
                                clock_ahead: None,
                            })
                            .collect();
                        self.refresh_clocks();
                        if read.compact && !self.compacting {
                            self.compact(library);
                        }
                    }
                    Err(error) => self.status.error = Some(error),
                }
            }
            Done::Compacted(result) => {
                self.compacting = false;
                if let Err(error) = result {
                    self.status.error = Some(error);
                }
            }
        }
    }

    fn compact(&mut self, library: &LibraryStore) {
        let Some(folder) = &self.folder else {
            return;
        };
        match library.sync_compaction() {
            Ok(Some((seq, covers, changes))) => {
                let work = Work::Compact {
                    journal: Journal::in_library(folder),
                    own: library.sync_device(),
                    seq,
                    covers,
                    changes,
                };
                self.compacting = self.requests.send(work).is_ok();
            }
            Ok(None) => {}
            Err(error) => self.status.error = Some(format!("{error:#}")),
        }
    }

    fn refresh_clocks(&mut self) {
        for device in &mut self.status.devices {
            device.clock_ahead = self
                .clock_ahead
                .get(&device.id)
                .map(|ms| Duration::from_millis(*ms));
        }
    }
}

/// The next changes to take in, about `limit` of them. A batch cut in two
/// marks its file read only with its last part, so a quit halfway reads
/// it again (taking a change twice changes nothing).
fn take_chunk(pending: &mut Vec<Batch>, limit: usize) -> Vec<Batch> {
    let mut chunk = Vec::new();
    let mut count = 0;
    while count < limit && !pending.is_empty() {
        let batch = &mut pending[0];
        let room = limit - count;
        if batch.changes.len() <= room {
            count += batch.changes.len();
            chunk.push(pending.remove(0));
        } else {
            let part = Batch {
                device: batch.device,
                seq: 0,
                covers: None,
                changes: batch.changes.drain(..room).collect(),
            };
            count += part.changes.len();
            chunk.push(part);
        }
    }
    chunk
}

fn run(work: Work) -> Done {
    match work {
        Work::Diff { snapshot, reader } => {
            Done::Diffed(Box::new(reader.diff(&snapshot).unwrap_or_else(|error| {
                log::warn!("sync diff failed: {error:#}");
                SyncExport::default()
            })))
        }
        Work::Write {
            journal,
            device,
            info,
            segments,
        } => Done::Written((|| {
            if journal
                .list_device(device)
                .map_err(|e| format!("{e:#}"))?
                .info
                .is_none()
            {
                journal
                    .write_device(device, &info)
                    .map_err(|error| format!("{error:#}"))?;
            }
            let mut written = Vec::new();
            for (seq, bytes) in segments {
                journal
                    .write_segment(device, seq, &bytes)
                    .map_err(|error| format!("Could not write to the library folder: {error:#}"))?;
                if let Some(digest) = Journal::digest_of(&bytes) {
                    written.push((seq, digest));
                }
            }
            Ok(written)
        })()),
        Work::Read {
            journal,
            own,
            marks,
            next_seq,
            written,
        } => Done::Read((|| {
            let devices = journal.devices().map_err(|error| format!("{error:#}"))?;
            let mine = devices.iter().find(|device| device.id == own);
            let forked = mine.is_some_and(|mine| mine.forked(next_seq, &written, &journal));
            let compact = mine.is_some_and(DeviceListing::should_compact);
            let batches = if forked {
                Vec::new()
            } else {
                journal
                    .pending(own, &marks, 20 * APPLY_PER_FRAME)
                    .map_err(|error| format!("{error:#}"))?
            };
            Ok(Read {
                batches,
                devices,
                forked,
                compact,
            })
        })()),
        Work::Compact {
            journal,
            own,
            seq,
            covers,
            changes,
        } => Done::Compacted(
            journal
                .compact(own, seq, glyphst_sync::wall_ms(), &covers, &changes)
                .map_err(|error| format!("{error:#}")),
        ),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(then: SystemTime) -> String {
    let seconds = SystemTime::now()
        .duration_since(then)
        .map_or(0, |elapsed| elapsed.as_secs());
    match seconds {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{} min ago", seconds / 60),
        3_600..86_400 => format!("{} h ago", seconds / 3_600),
        86_400..172_800 => "yesterday".to_string(),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

/// How far ahead a clock runs, roughly.
pub fn ahead(by: Duration) -> String {
    let minutes = by.as_secs() / 60;
    if minutes < 120 {
        format!("{minutes} min")
    } else if minutes < 48 * 60 {
        format!("{} h", minutes / 60)
    } else {
        format!("{} days", minutes / (24 * 60))
    }
}

/// Other computers in the library folder's journal (the folder is
/// joined, not just chosen, when there are).
pub fn computers_in(folder: &Path, own: DeviceId) -> Vec<(String, Option<SystemTime>)> {
    Journal::in_library(folder)
        .devices()
        .unwrap_or_default()
        .into_iter()
        .filter(|device| device.id != own)
        .map(|device| {
            (
                device
                    .info
                    .map(|info| info.name)
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "another computer".to_string()),
                device.last_written,
            )
        })
        .collect()
}

/// This computer's name, for the other machines' lists.
pub fn device_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this computer".to_string())
}

/// The library folder to sync: only one the user chose (the default one
/// in the data folder is no sync tool's).
pub fn synced_folder(library: &LibraryStore, enabled: bool) -> Option<PathBuf> {
    (enabled && library.files_root_chosen())
        .then(|| library.files_root().map(Path::to_path_buf))
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use glyphst_library::{
        DocumentId, HighlightColor, HighlightRect, LibraryImportOutcome, LibraryItemMetadata,
    };

    use super::*;

    struct Machine {
        store: LibraryStore,
        sync: LibrarySync,
        folder: PathBuf,
    }

    impl Machine {
        fn open(dir: &Path, name: &str) -> Self {
            let folder = dir.join(name);
            fs::create_dir_all(&folder).unwrap();
            Self {
                store: LibraryStore::open_for_test(dir.join(format!("{name}.redb"))).unwrap(),
                sync: quick_sync(),
                folder,
            }
        }

        /// Polls until the thread is idle and nothing waits.
        fn settle(&mut self) {
            for _ in 0..2_000 {
                self.sync.poll(&mut self.store, Some(self.folder.clone()));
                let idle = !self.sync.busy()
                    && !self.sync.inbound()
                    && self.sync.exported == Some(self.store.sync_revision())
                    && self.store.sync_outbox().unwrap().is_empty();
                if idle && self.sync.last_read.is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            panic!("the sync never settled: {:?}", self.sync.status);
        }

        fn item(&self, title: &str) -> Option<DocumentId> {
            self.store
                .library_items()
                .iter()
                .find(|item| item.title == title)
                .map(|item| item.id)
        }
    }

    fn quick_sync() -> LibrarySync {
        let mut sync = LibrarySync::new(&egui::Context::default());
        sync.export_after = Duration::ZERO;
        sync.read_every = Duration::from_millis(30);
        sync
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "glyphst-library-sync-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What a sync tool does: copies new and changed journal files from
    /// one library folder into the other, and removes ones gone there.
    fn carry(from: &Path, to: &Path) {
        let source = from.join("sync/devices");
        let Ok(devices) = fs::read_dir(&source) else {
            return;
        };
        for device in devices.flatten() {
            let target = to.join("sync/devices").join(device.file_name());
            fs::create_dir_all(&target).unwrap();
            let names = fs::read_dir(device.path())
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name())
                .collect::<BTreeSet<_>>();
            for name in &names {
                let bytes = fs::read(device.path().join(name)).unwrap();
                if fs::read(target.join(name)).ok().as_ref() != Some(&bytes) {
                    fs::write(target.join(name), bytes).unwrap();
                }
            }
            for entry in fs::read_dir(&target).unwrap().flatten() {
                if !names.contains(&entry.file_name()) {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        }
    }

    /// Rounds of syncing both, carrying each one's journal to the other.
    fn exchange(a: &mut Machine, b: &mut Machine) {
        for _ in 0..4 {
            a.settle();
            b.settle();
            carry(&a.folder, &b.folder);
            carry(&b.folder, &a.folder);
            a.sync.sync_now_for_tests();
            b.sync.sync_now_for_tests();
        }
        a.settle();
        b.settle();
    }

    fn link(store: &mut LibraryStore, path: &Path) -> DocumentId {
        match store
            .link_item(path, LibraryItemMetadata::for_path(path), None)
            .unwrap()
        {
            LibraryImportOutcome::Added(id) => id,
            other => panic!("{other:?}"),
        }
    }

    fn paper(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(format!("{name}.pdf"));
        fs::write(&path, format!("%PDF-1.4 {name}")).unwrap();
        path
    }

    fn rect() -> HighlightRect {
        HighlightRect {
            page_idx: 0,
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0,
        }
    }

    #[test]
    fn two_computers_sync_through_their_library_folders() {
        let dir = folder("folders");
        let (mut a, mut b) = (Machine::open(&dir, "a"), Machine::open(&dir, "b"));
        let file = paper(&dir, "paper");
        let id = link(&mut a.store, &file);
        a.store
            .add_highlight(&file, HighlightColor::Green, vec![rect()], "seen", "")
            .unwrap();
        exchange(&mut a, &mut b);
        let on_b = b.item("paper").expect("the paper reached b");
        assert_eq!(b.store.highlights_for(&file).len(), 1);

        b.store.set_library_item_favorite(on_b, true).unwrap();
        exchange(&mut a, &mut b);
        assert!(a.store.library_item(id).unwrap().favorite);
        let names = |machine: &Machine| {
            machine
                .sync
                .status
                .devices
                .iter()
                .filter(|device| !device.this)
                .count()
        };
        assert_eq!((names(&a), names(&b)), (1, 1));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_export_left_in_the_outbox_by_a_crash_is_written_next_time() {
        let dir = folder("crash");
        let (mut a, mut b) = (Machine::open(&dir, "a"), Machine::open(&dir, "b"));
        link(&mut a.store, &paper(&dir, "paper"));
        // Exported, then the app stopped before the segment was written.
        assert!(!a.store.export_now().unwrap().is_empty());
        drop(std::mem::replace(&mut a.sync, quick_sync()));
        exchange(&mut a, &mut b);
        assert!(b.item("paper").is_some());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_computer_that_joins_late_reads_the_compacted_journal() {
        let dir = folder("compact");
        let (mut a, mut b) = (Machine::open(&dir, "a"), Machine::open(&dir, "b"));
        let id = link(&mut a.store, &paper(&dir, "paper"));
        // Many small exports, each a segment.
        for round in 0..(glyphst_sync::journal::COMPACT_SEGMENTS + 5) {
            a.store
                .set_library_item_favorite(id, round % 2 == 0)
                .unwrap();
            a.store.export_now().unwrap();
        }
        a.settle();
        // The next read sees the journal is long and compacts it.
        a.sync.sync_now_for_tests();
        a.settle();
        let listing = Journal::in_library(&a.folder)
            .list_device(a.store.sync_device())
            .unwrap();
        assert_eq!(listing.snapshots.len(), 1, "{listing:?}");
        assert!(listing.segments.len() < 5);

        exchange(&mut a, &mut b);
        let on_b = b.item("paper").expect("the late computer got the paper");
        assert_eq!(
            b.store.library_item(on_b).unwrap().favorite,
            a.store.library_item(id).unwrap().favorite
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_copied_database_becomes_a_new_computer() {
        let dir = folder("fork");
        let mut a = Machine::open(&dir, "a");
        link(&mut a.store, &paper(&dir, "first"));
        a.settle();
        // Another computer starts from a copy of a's database.
        fs::copy(dir.join("a.redb"), dir.join("copy.redb")).unwrap();
        let mut copy = Machine::open(&dir, "copy");
        assert_eq!(copy.store.sync_device(), a.store.sync_device());
        carry(&a.folder, &copy.folder);

        link(&mut a.store, &paper(&dir, "second"));
        a.settle();
        link(&mut copy.store, &paper(&dir, "third"));
        carry(&a.folder, &copy.folder);
        copy.settle();
        copy.sync.sync_now_for_tests();
        copy.settle();
        assert_ne!(copy.store.sync_device(), a.store.sync_device());

        exchange(&mut a, &mut copy);
        for title in ["first", "second", "third"] {
            assert!(a.item(title).is_some(), "{title} on a");
            assert!(copy.item(title).is_some(), "{title} on the copy");
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
