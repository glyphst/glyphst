//! The reading activity log: what was opened, read (with how long),
//! highlighted, noted, asked, cited and translated, newest last. Home
//! shows it as the activity pane and sums the reading time from it. Kept
//! in its own table, `activity_v1`, capped at [`MAX_ACTIVITY`] events.

use super::storage_helpers::*;
use super::*;

/// The most events kept; older ones are dropped as new ones arrive. Tests
/// keep fewer, so filling the log stays quick.
pub const MAX_ACTIVITY: usize = if cfg!(test) { 40 } else { 5_000 };

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActivityKind {
    Open,
    /// A span of reading; [`ActivityEvent::seconds`] says how long.
    Read,
    Highlight,
    Note,
    Bookmark,
    Ask,
    Cite,
    Translate,
}

impl ActivityKind {
    /// The verb the activity pane shows.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Read => "read",
            Self::Highlight => "mark",
            Self::Note => "note",
            Self::Bookmark => "mark",
            Self::Ask => "ask",
            Self::Cite => "cite",
            Self::Translate => "trans",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityEvent {
    /// The event's key, increasing.
    pub id: u64,
    /// When, in seconds since the epoch.
    pub at: u64,
    pub kind: ActivityKind,
    pub path: PathBuf,
    /// A short description: `p.4`, a question, `en→zh · 16p`.
    pub detail: String,
    /// For reading spans, how long.
    pub seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredActivityV1 {
    at: u64,
    kind: ActivityKind,
    path: StoredPathV1,
    detail: String,
    seconds: u64,
}

impl StoredActivityV1 {
    fn from_event(event: &ActivityEvent) -> Self {
        Self {
            at: event.at,
            kind: event.kind,
            path: StoredPathV1::from_path(&event.path),
            detail: event.detail.clone(),
            seconds: event.seconds,
        }
    }

    fn into_event(self, id: u64) -> Result<ActivityEvent> {
        Ok(ActivityEvent {
            id,
            at: self.at,
            kind: self.kind,
            path: self.path.into_path()?,
            detail: self.detail,
            seconds: self.seconds,
        })
    }
}

impl LibraryStore {
    /// Every event kept, oldest first.
    pub fn activity(&self) -> &[ActivityEvent] {
        &self.library.activity
    }

    /// Adds an event now.
    pub fn record_activity(
        &mut self,
        kind: ActivityKind,
        path: &Path,
        detail: impl Into<String>,
        seconds: u64,
    ) -> Result<()> {
        self.record_activity_at(kind, path, detail, seconds, current_timestamp())
    }

    /// Adds an event at `at` (seconds since the epoch).
    pub fn record_activity_at(
        &mut self,
        kind: ActivityKind,
        path: &Path,
        detail: impl Into<String>,
        seconds: u64,
        at: u64,
    ) -> Result<()> {
        let id = self.library.activity.last().map_or(1, |last| last.id + 1);
        let event = ActivityEvent {
            id,
            at,
            kind,
            path: normalize_path(path),
            detail: detail.into(),
            seconds,
        };
        let dropped: Vec<u64> = {
            let activity = &mut self.library.activity;
            activity.push(event.clone());
            let over = activity.len().saturating_sub(MAX_ACTIVITY);
            activity.drain(..over).map(|event| event.id).collect()
        };
        if let Some(database) = &self.database {
            let write_txn = database.begin_write()?;
            {
                let mut table = write_txn.open_table(ACTIVITY_V1_TABLE)?;
                let encoded = encode(&StoredActivityV1::from_event(&event))?;
                table.insert(event.id, encoded.as_slice())?;
                for id in dropped {
                    table.remove(id)?;
                }
            }
            write_txn.commit()?;
        }
        Ok(())
    }

    /// Seconds spent reading since `since` (seconds since the epoch), in
    /// every document or in `path`'s.
    pub fn reading_seconds(&self, since: u64, path: Option<&Path>) -> u64 {
        let path = path.map(normalize_path);
        self.library
            .activity
            .iter()
            .filter(|event| event.kind == ActivityKind::Read && event.at >= since)
            .filter(|event| path.as_ref().is_none_or(|path| &event.path == path))
            .map(|event| event.seconds)
            .sum()
    }
}

/// Loads the log, oldest first.
pub(super) fn load_activity(read_txn: &redb::ReadTransaction, library: &mut Library) -> Result<()> {
    let table = read_txn.open_table(ACTIVITY_V1_TABLE)?;
    for entry in table.iter()? {
        let (key, value) = entry?;
        match decode::<StoredActivityV1>(value.value())
            .and_then(|stored| stored.into_event(key.value()))
        {
            Ok(mut event) => {
                event.path = normalize_path(&event.path);
                library.activity.push(event);
            }
            Err(error) => eprintln!("failed to decode an activity event: {error:#}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (LibraryStore, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("glyphst-activity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("library.redb");
        (LibraryStore::open_for_test(path.clone()).unwrap(), path)
    }

    #[test]
    fn events_persist_in_order_and_sum_reading_time() {
        let (mut library, path) = store("persist");
        let paper = Path::new("/papers/attention.pdf");
        let book = Path::new("/books/sicp.epub");
        library
            .record_activity_at(ActivityKind::Open, paper, "p.4", 0, 1_000)
            .unwrap();
        library
            .record_activity_at(ActivityKind::Read, paper, "", 600, 1_600)
            .unwrap();
        library
            .record_activity_at(ActivityKind::Read, book, "", 300, 2_000)
            .unwrap();
        library
            .record_activity_at(ActivityKind::Highlight, paper, "p.4 ¶3", 0, 2_100)
            .unwrap();
        drop(library);
        let library = LibraryStore::open_for_test(path).unwrap();
        let kinds: Vec<ActivityKind> = library.activity().iter().map(|event| event.kind).collect();
        assert_eq!(
            kinds,
            [
                ActivityKind::Open,
                ActivityKind::Read,
                ActivityKind::Read,
                ActivityKind::Highlight
            ]
        );
        assert_eq!(library.activity()[3].detail, "p.4 ¶3");
        assert_eq!(library.reading_seconds(0, None), 900);
        assert_eq!(library.reading_seconds(0, Some(paper)), 600);
        assert_eq!(library.reading_seconds(1_700, None), 300);
    }

    #[test]
    fn the_log_keeps_only_the_newest_events() {
        let (mut library, path) = store("cap");
        let paper = Path::new("/papers/a.pdf");
        for at in 0..(MAX_ACTIVITY as u64 + 3) {
            library
                .record_activity_at(ActivityKind::Open, paper, at.to_string(), 0, at)
                .unwrap();
        }
        assert_eq!(library.activity().len(), MAX_ACTIVITY);
        assert_eq!(library.activity()[0].detail, "3");
        drop(library);
        let library = LibraryStore::open_for_test(path).unwrap();
        assert_eq!(library.activity().len(), MAX_ACTIVITY);
        assert_eq!(library.activity()[0].detail, "3");
    }
}
