//! Reading sessions: each span of reading with when it started, how long
//! it ran and the pages it went over, oldest first. Home's heatmap and the
//! reading activity tab bucket them into days. Kept in their own table,
//! `reading_sessions_v1`, without the activity log's cap, so a year of
//! reading stays whole; an empty table starts from the activity log's
//! reading spans.

use super::storage_helpers::*;
use super::*;

/// A span of reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingSession {
    /// The session's key, increasing.
    pub id: u64,
    /// When it started, in seconds since the epoch.
    pub start: u64,
    pub seconds: u64,
    pub path: PathBuf,
    /// The first and last page shown, numbered from 1, when known.
    pub pages: Option<(usize, usize)>,
}

impl ReadingSession {
    pub fn end(&self) -> u64 {
        self.start + self.seconds
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReadingSessionV1 {
    start: u64,
    seconds: u64,
    path: StoredPathV1,
    pages: Option<(usize, usize)>,
}

impl StoredReadingSessionV1 {
    fn from_session(session: &ReadingSession) -> Self {
        Self {
            start: session.start,
            seconds: session.seconds,
            path: StoredPathV1::from_path(&session.path),
            pages: session.pages,
        }
    }

    fn into_session(self, id: u64) -> Result<ReadingSession> {
        Ok(ReadingSession {
            id,
            start: self.start,
            seconds: self.seconds,
            path: self.path.into_path()?,
            pages: self.pages,
        })
    }
}

impl LibraryStore {
    /// Every reading session, oldest first.
    pub fn reading_sessions(&self) -> &[ReadingSession] {
        &self.library.reading_sessions
    }

    /// Adds a session that started at `start` (seconds since the epoch).
    pub fn record_reading(
        &mut self,
        path: &Path,
        start: u64,
        seconds: u64,
        pages: Option<(usize, usize)>,
    ) -> Result<()> {
        let sessions = &mut self.library.reading_sessions;
        // Sessions taken from the activity log are stored with the first
        // one recorded after them.
        let mut next = sessions.iter().map(|session| session.id).max().unwrap_or(0) + 1;
        let mut unsaved = Vec::new();
        for session in sessions.iter_mut().filter(|session| session.id == 0) {
            session.id = next;
            next += 1;
            unsaved.push(session.clone());
        }
        let session = ReadingSession {
            id: next,
            start,
            seconds,
            path: normalize_path(path),
            pages,
        };
        unsaved.push(session.clone());
        if let Some(database) = &self.database {
            let write_txn = database.begin_write()?;
            {
                let mut table = write_txn.open_table(READING_SESSIONS_V1_TABLE)?;
                for session in &unsaved {
                    let encoded = encode(&StoredReadingSessionV1::from_session(session))?;
                    table.insert(session.id, encoded.as_slice())?;
                }
            }
            write_txn.commit()?;
        }
        let at = sessions.partition_point(|other| other.start <= session.start);
        sessions.insert(at, session);
        Ok(())
    }
}

/// Loads the sessions, oldest first; with none stored yet, the activity
/// log's reading spans stand in for them (the log is loaded first).
pub(super) fn load_reading_sessions(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
) -> Result<()> {
    let table = read_txn.open_table(READING_SESSIONS_V1_TABLE)?;
    for entry in table.iter()? {
        let (key, value) = entry?;
        match decode::<StoredReadingSessionV1>(value.value())
            .and_then(|stored| stored.into_session(key.value()))
        {
            Ok(mut session) => {
                session.path = normalize_path(&session.path);
                library.reading_sessions.push(session);
            }
            Err(error) => eprintln!("failed to decode a reading session: {error:#}"),
        }
    }
    if library.reading_sessions.is_empty() {
        library.reading_sessions = sessions_from_activity(&library.activity);
    }
    library
        .reading_sessions
        .sort_by_key(|session| session.start);
    Ok(())
}

/// Reading spans of the activity log as sessions: a span is logged when
/// it ends. They are kept in memory only (id 0) until the first session
/// is recorded after them.
fn sessions_from_activity(activity: &[ActivityEvent]) -> Vec<ReadingSession> {
    activity
        .iter()
        .filter(|event| event.kind == ActivityKind::Read)
        .map(|event| ReadingSession {
            id: 0,
            start: event.at.saturating_sub(event.seconds),
            seconds: event.seconds,
            path: event.path.clone(),
            pages: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (LibraryStore, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("glyphst-reading-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("library.redb");
        (LibraryStore::open_for_test(path.clone()).unwrap(), path)
    }

    #[test]
    fn sessions_persist_in_start_order() {
        let (mut library, path) = store("persist");
        let paper = Path::new("/papers/attention.pdf");
        library
            .record_reading(paper, 2_000, 600, Some((4, 6)))
            .unwrap();
        library.record_reading(paper, 1_000, 300, None).unwrap();
        drop(library);
        let library = LibraryStore::open_for_test(path).unwrap();
        let sessions = library.reading_sessions();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].start, 1_000);
        assert_eq!(sessions[1].pages, Some((4, 6)));
        assert_eq!(sessions[1].end(), 2_600);
    }

    #[test]
    fn an_empty_table_starts_from_the_activity_logs_reading_spans() {
        let (mut library, path) = store("seed");
        let paper = Path::new("/papers/attention.pdf");
        library
            .record_activity_at(ActivityKind::Open, paper, "p.1", 0, 900)
            .unwrap();
        library
            .record_activity_at(ActivityKind::Read, paper, "", 600, 1_600)
            .unwrap();
        drop(library);
        let mut library = LibraryStore::open_for_test(path.clone()).unwrap();
        let sessions = library.reading_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!((sessions[0].start, sessions[0].seconds), (1_000, 600));
        // Recording a session stores the ones taken from the log with it.
        library.record_reading(paper, 5_000, 60, None).unwrap();
        drop(library);
        let library = LibraryStore::open_for_test(path).unwrap();
        let ids: Vec<u64> = library.reading_sessions().iter().map(|s| s.id).collect();
        assert_eq!(ids, [1, 2]);
    }
}
