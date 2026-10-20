//! The library's side of syncing between machines: stable ids for its
//! records (`ids`), the records as the journal carries them, named only by
//! uids and names that are the same on every machine (`canonical`), and
//! what this machine last exported or took from others, field by field
//! (`registers`), from which the next export is the difference.

mod apply;
mod canonical;
mod ids;
mod registers;

use std::collections::BTreeMap;
use std::sync::atomic::AtomicU64;

use glyphst_sync::{Clock, DeviceId, Hlc, Watermarks};

pub use apply::ApplyReport;
pub use canonical::{SyncSnapshot, canonical, kinds};
pub(super) use ids::{SYNC_IDS_V1_TABLE, SYNC_STATE_V1_TABLE};
pub use ids::{SyncIds, SyncKind};
pub use registers::{Export, SyncReader};
pub(super) use registers::{SYNC_OUTBOX_V1_TABLE, SYNC_REGISTERS_V1_TABLE};

use super::storage_helpers::{decode, encode};
use super::*;

const STATE_KEY: &str = "state";
const SYNC_STORAGE_VERSION: u8 = 1;
/// Digests of this machine's last segments kept, to notice another writer.
const WRITTEN_KEPT: usize = 64;

/// This machine's part in syncing, kept with the library.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct SyncState {
    pub(super) device: DeviceId,
    /// The last time the clock gave out.
    pub(super) hlc: Option<Hlc>,
    /// The number of the next segment this machine writes.
    pub(super) next_seq: u64,
    /// Other machines' segments read through.
    #[serde(default)]
    pub(super) marks: Watermarks,
    /// The digests of this machine's segments as written.
    #[serde(default)]
    pub(super) written: BTreeMap<u64, String>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            device: DeviceId::new(),
            hlc: None,
            next_seq: 1,
            marks: Watermarks::new(),
            written: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredSyncStateV1 {
    version: u8,
    state: SyncState,
}

/// The store's sync fields.
#[derive(Debug, Default)]
pub(super) struct SyncFields {
    pub(super) ids: SyncIds,
    pub(super) revision: AtomicU64,
    pub(super) state: SyncState,
    /// Bumped whenever others' changes are applied: an export worked out
    /// before then is stale.
    pub(super) generation: u64,
    /// When each item (with its AI results) last changed, by the clock:
    /// what its vault note was written from.
    pub(super) item_revs: HashMap<Uuid, Hlc>,
}

pub(super) fn load_sync(database: &Database) -> Result<SyncFields> {
    let ids = ids::load_sync_ids(database)?;
    let stored = {
        let read_txn = database.begin_read()?;
        let table = read_txn.open_table(SYNC_STATE_V1_TABLE)?;
        table.get(STATE_KEY)?.map(|value| value.value().to_vec())
    };
    let state = match stored {
        Some(bytes) => match decode::<StoredSyncStateV1>(&bytes).and_then(|stored| {
            require_storage_version(stored.version, SYNC_STORAGE_VERSION)?;
            Ok(stored.state)
        }) {
            Ok(state) => state,
            Err(error) => {
                // Starting over as a new device is safe: others' changes
                // are read again and merge the same.
                eprintln!("failed to decode the sync state, starting a new one: {error:#}");
                SyncState::default()
            }
        },
        None => SyncState::default(),
    };
    let write_txn = database.begin_write()?;
    write_state_in(&write_txn, &state)?;
    write_txn.commit()?;
    let item_revs = registers::item_revs(database)?;
    Ok(SyncFields {
        ids,
        revision: AtomicU64::new(0),
        state,
        generation: 0,
        item_revs,
    })
}

fn write_state_in(write_txn: &redb::WriteTransaction, state: &SyncState) -> Result<()> {
    let mut table = write_txn.open_table(SYNC_STATE_V1_TABLE)?;
    let encoded = encode(&StoredSyncStateV1 {
        version: SYNC_STORAGE_VERSION,
        state: state.clone(),
    })?;
    table.insert(STATE_KEY, encoded.as_slice())?;
    Ok(())
}

impl LibraryStore {
    /// This machine in the journal.
    pub fn sync_device(&self) -> DeviceId {
        self.sync.state.device
    }

    fn sync_clock(&self) -> Clock {
        Clock::new(self.sync.state.device, self.sync.state.hlc)
    }

    /// The number of the next segment this machine writes.
    pub fn sync_next_seq(&self) -> u64 {
        self.sync.state.next_seq
    }

    /// Others' segments read through.
    pub fn sync_watermarks(&self) -> &Watermarks {
        &self.sync.state.marks
    }

    /// The digests of this machine's last segments as written.
    pub fn sync_written(&self) -> &BTreeMap<u64, String> {
        &self.sync.state.written
    }

    /// When the item (with its AI results) last changed by the sync's
    /// clock; `None` while it never synced.
    pub fn sync_item_rev(&self, id: DocumentId) -> Option<Hlc> {
        let uid = self.sync.ids.uid(SyncKind::Item, id.0)?;
        self.sync.item_revs.get(&uid).copied()
    }

    pub(super) fn note_item_rev(&mut self, kind: &str, key: &str, hlc: Hlc) {
        if let Some(uid) = registers::rev_item(kind, key) {
            let rev = self.sync.item_revs.entry(uid).or_insert(hlc);
            *rev = (*rev).max(hlc);
        }
    }
}

#[cfg(test)]
mod tests;
