//! Background work on catalog items (metadata lookups, the AI pass),
//! queued in the library so it carries on after a restart.

use super::storage_helpers::{decode, encode};
use super::*;

pub(super) const LIBRARY_JOBS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_jobs_v1");
const LIBRARY_JOBS_STORAGE_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LibraryJobId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LibraryJobKind {
    /// Find identifiers in the file and look its metadata up.
    Metadata,
    /// Summarise, tag and file the item with the library model.
    Enrich,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryJobOrigin {
    /// The item was just added.
    OnAdd,
    /// The user asked to run over the existing library.
    Backfill,
    /// The user asked for this item.
    Manual,
    /// An assistant asked.
    Agent,
    /// An import brought the item in.
    Import,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryJobState {
    Queued,
    Running,
    /// Waiting to retry after a failure, until this time (seconds).
    Waiting {
        until: u64,
    },
    Failed {
        error: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryJob {
    pub id: LibraryJobId,
    pub item_id: DocumentId,
    pub kind: LibraryJobKind,
    pub origin: LibraryJobOrigin,
    pub state: LibraryJobState,
    pub attempts: u32,
    /// Run even when the item's input has not changed since the last run.
    pub force: bool,
    pub created: u64,
}

impl LibraryJob {
    pub fn is_ready(&self, now: u64) -> bool {
        match self.state {
            LibraryJobState::Queued => true,
            LibraryJobState::Waiting { until } => until <= now,
            LibraryJobState::Running | LibraryJobState::Failed { .. } => false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredLibraryJobV1 {
    version: u8,
    job: LibraryJob,
}

pub(super) fn ensure_jobs_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    write_txn.open_table(LIBRARY_JOBS_V1_TABLE)?;
    Ok(())
}

/// The queue as saved; a job that was running when the Reader stopped is
/// queued again.
pub(super) fn load_library_jobs(database: &Database) -> Result<Vec<LibraryJob>> {
    let read_txn = database.begin_read()?;
    let table = read_txn.open_table(LIBRARY_JOBS_V1_TABLE)?;
    let mut jobs = Vec::new();
    for entry in table.iter()? {
        let (_, value) = entry?;
        match decode::<StoredLibraryJobV1>(value.value()).and_then(|stored| {
            require_storage_version(stored.version, LIBRARY_JOBS_STORAGE_VERSION)?;
            Ok(stored.job)
        }) {
            Ok(mut job) => {
                if job.state == LibraryJobState::Running {
                    job.state = LibraryJobState::Queued;
                }
                jobs.push(job);
            }
            Err(error) => eprintln!("failed to decode library job: {error:#}"),
        }
    }
    jobs.sort_by_key(|job| job.id);
    Ok(jobs)
}

impl LibraryStore {
    pub fn library_jobs(&self) -> &[LibraryJob] {
        &self.jobs
    }

    /// Queues a job of this kind for each item that has none waiting.
    /// Returns the jobs added.
    pub fn enqueue_library_jobs(
        &mut self,
        items: &[DocumentId],
        kind: LibraryJobKind,
        origin: LibraryJobOrigin,
        force: bool,
    ) -> Result<Vec<LibraryJobId>> {
        let now = current_timestamp();
        let mut added = Vec::new();
        for item_id in items {
            if self.library_item(*item_id).is_none()
                || self.jobs.iter().any(|job| {
                    job.item_id == *item_id
                        && job.kind == kind
                        && !matches!(job.state, LibraryJobState::Failed { .. })
                })
            {
                continue;
            }
            self.jobs
                .retain(|job| !(job.item_id == *item_id && job.kind == kind));
            let id = LibraryJobId(self.next_job_id.max(1));
            self.next_job_id = id.0.saturating_add(1);
            self.jobs.push(LibraryJob {
                id,
                item_id: *item_id,
                kind,
                origin,
                state: LibraryJobState::Queued,
                attempts: 0,
                force,
                created: now,
            });
            added.push(id);
        }
        self.write_library_jobs()?;
        Ok(added)
    }

    /// Up to `limit` jobs of this kind that can run now, oldest first,
    /// marked running.
    pub fn take_library_jobs(&mut self, kind: LibraryJobKind, limit: usize) -> Vec<LibraryJob> {
        let now = current_timestamp();
        let mut taken = Vec::new();
        for job in self
            .jobs
            .iter_mut()
            .filter(|job| job.kind == kind && job.is_ready(now))
            .take(limit)
        {
            job.state = LibraryJobState::Running;
            job.attempts = job.attempts.saturating_add(1);
            taken.push(job.clone());
        }
        if !taken.is_empty()
            && let Err(error) = self.write_library_jobs()
        {
            eprintln!("failed to save the library queue: {error:#}");
        }
        taken
    }

    /// Ends a job: done removes it; otherwise it waits or has failed.
    pub fn finish_library_job(
        &mut self,
        id: LibraryJobId,
        outcome: Option<LibraryJobState>,
    ) -> Result<()> {
        match outcome {
            None => self.jobs.retain(|job| job.id != id),
            Some(state) => {
                if let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) {
                    job.state = state;
                }
            }
        }
        self.write_library_jobs()
    }

    /// Puts running jobs back in the queue (the worker stopped).
    pub fn requeue_running_library_jobs(&mut self) -> Result<()> {
        let mut changed = false;
        for job in &mut self.jobs {
            if job.state == LibraryJobState::Running {
                job.state = LibraryJobState::Queued;
                changed = true;
            }
        }
        if changed {
            self.write_library_jobs()?;
        }
        Ok(())
    }

    /// Drops queued jobs of this kind (all kinds with `None`).
    pub fn cancel_library_jobs(&mut self, kind: Option<LibraryJobKind>) -> Result<usize> {
        let before = self.jobs.len();
        self.jobs.retain(|job| {
            job.state == LibraryJobState::Running || kind.is_some_and(|kind| job.kind != kind)
        });
        let removed = before - self.jobs.len();
        if removed > 0 {
            self.write_library_jobs()?;
        }
        Ok(removed)
    }

    pub(super) fn prune_library_jobs(&mut self) {
        let before = self.jobs.len();
        let items = self
            .library
            .library_items
            .iter()
            .filter(|item| item.trashed_at.is_none())
            .map(|item| item.id)
            .collect::<HashSet<_>>();
        self.jobs.retain(|job| items.contains(&job.item_id));
        if self.jobs.len() != before
            && let Err(error) = self.write_library_jobs()
        {
            eprintln!("failed to save the library queue: {error:#}");
        }
    }

    fn write_library_jobs(&self) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(LIBRARY_JOBS_V1_TABLE)?;
            let retained = self.jobs.iter().map(|job| job.id.0).collect::<HashSet<_>>();
            let stale = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
                .filter(|id| !retained.contains(id))
                .collect::<Vec<_>>();
            for id in stale {
                table.remove(id)?;
            }
            for job in &self.jobs {
                let encoded = encode(&StoredLibraryJobV1 {
                    version: LIBRARY_JOBS_STORAGE_VERSION,
                    job: job.clone(),
                })?;
                table.insert(job.id.0, encoded.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }
}
