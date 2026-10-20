//! Each synced record's fields as this machine last exported them or took
//! them from another, with when they were set: the baseline the next
//! export is the difference from, and the clock merges compare with. An
//! export is kept in the outbox, in the same transaction, until its
//! segment is written into the journal, so a crash between the two loses
//! nothing and sends nothing twice.

use std::collections::{BTreeMap, HashMap};

use glyphst_sync::{Change, DELETED, Fields, Hlc, RecordKey, Register, journal, wall_ms};
use serde_json::Value;

use super::canonical::kinds;
use super::*;

/// `<kind>/<key>` → the record's registers.
pub(in crate::library) const SYNC_REGISTERS_V1_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("sync_registers_v1");
/// Segment number → the segment as it is to be written.
pub(in crate::library) const SYNC_OUTBOX_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("sync_outbox_v1");

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct StoredRecordV1 {
    pub(super) version: u8,
    /// Whether the record is (or was) in this machine's library: only such
    /// a record's absence is a removal. A record taken from another
    /// machine and still waiting (its file has not arrived) is not.
    #[serde(default)]
    pub(super) materialized: bool,
    /// Another machine's record for one this machine has under its own
    /// uid: never materialized, never a removal.
    #[serde(default)]
    pub(super) aliased: bool,
    pub(super) fields: BTreeMap<String, Register>,
}

pub(super) fn row_key(key: &RecordKey) -> String {
    format!("{}/{}", key.kind, key.key)
}

fn parse_row_key(text: &str) -> Option<RecordKey> {
    let (kind, key) = text.split_once('/')?;
    Some(RecordKey::new(kind, key))
}

/// The item a record's changes show in its vault note, if any.
pub(super) fn rev_item(kind: &str, key: &str) -> Option<Uuid> {
    match kind {
        kinds::ITEM | kinds::ENRICHMENT | kinds::METADATA => Uuid::parse_str(key).ok(),
        kinds::ASSIGNMENT => Uuid::parse_str(key.split('|').next()?).ok(),
        _ => None,
    }
}

/// Each item's newest change, from the registers (none while the
/// library never synced).
pub(super) fn item_revs(database: &Database) -> Result<HashMap<Uuid, Hlc>> {
    let read_txn = database.begin_read()?;
    let table = read_txn.open_table(SYNC_REGISTERS_V1_TABLE)?;
    let mut revs = HashMap::<Uuid, Hlc>::new();
    for entry in table.iter()? {
        let (key, value) = entry?;
        let Some(record) = parse_row_key(key.value()) else {
            continue;
        };
        let Some(uid) = rev_item(&record.kind, &record.key) else {
            continue;
        };
        let Ok(stored) = decode::<StoredRecordV1>(value.value()) else {
            continue;
        };
        if let Some(newest) = stored.fields.values().map(|register| register.hlc).max() {
            let rev = revs.entry(uid).or_insert(newest);
            *rev = (*rev).max(newest);
        }
    }
    Ok(revs)
}

/// What changed since the last export, field by field (a field gone is
/// `null`; a record gone is [`DELETED`]).
#[derive(Clone, Debug, Default)]
pub struct Export {
    pub changes: Vec<(RecordKey, Fields)>,
    pub(super) generation: u64,
}

impl Export {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Reads the registers off the UI thread.
#[derive(Clone)]
pub struct SyncReader {
    database: Arc<Database>,
}

impl LibraryStore {
    pub fn sync_reader(&self) -> Option<SyncReader> {
        self.database
            .clone()
            .map(|database| SyncReader { database })
    }
}

impl SyncReader {
    pub(super) fn registers(&self) -> Result<HashMap<RecordKey, StoredRecordV1>> {
        let read_txn = self.database.begin_read()?;
        let table = read_txn.open_table(SYNC_REGISTERS_V1_TABLE)?;
        let mut registers = HashMap::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            let Some(record) = parse_row_key(key.value()) else {
                continue;
            };
            match decode::<StoredRecordV1>(value.value()).and_then(|stored| {
                require_storage_version(stored.version, SYNC_STORAGE_VERSION)?;
                Ok(stored)
            }) {
                Ok(stored) => {
                    registers.insert(record, stored);
                }
                Err(error) => eprintln!("failed to decode sync registers: {error:#}"),
            }
        }
        Ok(registers)
    }

    /// What `snapshot` changes from the registers.
    pub fn diff(&self, snapshot: &SyncSnapshot) -> Result<Export> {
        let records = canonical(snapshot);
        Ok(Export {
            changes: diff(&records, &self.registers()?),
            generation: snapshot.generation,
        })
    }
}

fn is_deleted(stored: &StoredRecordV1) -> bool {
    stored
        .fields
        .get(DELETED)
        .is_some_and(|register| register.value == Value::Bool(true))
}

fn diff(
    records: &BTreeMap<RecordKey, Fields>,
    registers: &HashMap<RecordKey, StoredRecordV1>,
) -> Vec<(RecordKey, Fields)> {
    let mut changes = Vec::new();
    for (key, fields) in records {
        let stored = registers.get(key);
        let mut changed = Fields::new();
        for (field, value) in fields {
            let current = stored.and_then(|stored| stored.fields.get(field));
            let same = match current {
                Some(register) => &register.value == value,
                None => value.is_null(),
            };
            if !same {
                changed.insert(field.clone(), value.clone());
            }
        }
        if let Some(stored) = stored {
            for (field, register) in &stored.fields {
                if !fields.contains_key(field) && !register.value.is_null() {
                    // Gone: a membership left, an optional field cleared,
                    // or (`deleted`) a record back after its removal.
                    changed.insert(field.clone(), Value::Null);
                }
            }
        }
        if !changed.is_empty() {
            changes.push((key.clone(), changed));
        }
    }
    let mut removed = registers
        .iter()
        .filter(|(key, stored)| {
            stored.materialized
                && !stored.aliased
                && !is_deleted(stored)
                && !records.contains_key(*key)
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    removed.sort();
    for key in removed {
        changes.push((
            key,
            Fields::from([(DELETED.to_string(), Value::Bool(true))]),
        ));
    }
    changes
}

impl LibraryStore {
    /// Takes an export into the registers and the outbox, each record's
    /// changes at a new time: returns the segments to write, by number.
    /// An export worked out before others' changes were applied is
    /// dropped (the next one sees them).
    pub fn commit_export(&mut self, export: Export) -> Result<Vec<u64>> {
        if export.generation != self.sync.generation || export.changes.is_empty() {
            return Ok(Vec::new());
        }
        let Some(database) = self.database.clone() else {
            return Ok(Vec::new());
        };
        let mut clock = self.sync_clock();
        let wall = wall_ms();
        let changes = export
            .changes
            .into_iter()
            .map(|(key, fields)| Change {
                hlc: clock.now(wall),
                kind: key.kind,
                key: key.key,
                fields,
            })
            .collect::<Vec<_>>();
        let mut state = self.sync.state.clone();
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(SYNC_REGISTERS_V1_TABLE)?;
            for change in &changes {
                let key = row_key(&change.record());
                let mut stored = table
                    .get(key.as_str())?
                    .and_then(|value| decode::<StoredRecordV1>(value.value()).ok())
                    .unwrap_or_default();
                stored.version = SYNC_STORAGE_VERSION;
                stored.materialized = true;
                for (field, value) in &change.fields {
                    stored.fields.insert(
                        field.clone(),
                        Register {
                            hlc: change.hlc,
                            value: value.clone(),
                        },
                    );
                }
                table.insert(key.as_str(), encode(&stored)?.as_slice())?;
            }
        }
        let revs = changes
            .iter()
            .map(|change| (change.kind.clone(), change.key.clone(), change.hlc))
            .collect::<Vec<_>>();
        let mut seqs = Vec::new();
        {
            let mut outbox = write_txn.open_table(SYNC_OUTBOX_V1_TABLE)?;
            for segment in journal::split_changes(changes) {
                let seq = state.next_seq;
                state.next_seq += 1;
                let bytes = journal::encode_segment(state.device, seq, wall, &segment);
                outbox.insert(seq, bytes.as_slice())?;
                seqs.push(seq);
            }
        }
        state.hlc = Some(clock.last());
        write_state_in(&write_txn, &state)?;
        write_txn.commit()?;
        self.sync.state = state;
        for (kind, key, hlc) in revs {
            self.note_item_rev(&kind, &key, hlc);
        }
        Ok(seqs)
    }

    /// Segments exported and not yet written into the journal.
    pub fn sync_outbox(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let Some(database) = &self.database else {
            return Ok(Vec::new());
        };
        let read_txn = database.begin_read()?;
        let table = read_txn.open_table(SYNC_OUTBOX_V1_TABLE)?;
        let mut segments = Vec::new();
        for entry in table.iter()? {
            let (seq, bytes) = entry?;
            segments.push((seq.value(), bytes.value().to_vec()));
        }
        Ok(segments)
    }

    /// The segment is in the journal: out of the outbox, its digest kept.
    pub fn sync_segment_written(&mut self, seq: u64, digest: String) -> Result<()> {
        let Some(database) = self.database.clone() else {
            return Ok(());
        };
        let mut state = self.sync.state.clone();
        state.written.insert(seq, digest);
        while state.written.len() > WRITTEN_KEPT {
            state.written.pop_first();
        }
        let write_txn = database.begin_write()?;
        write_txn.open_table(SYNC_OUTBOX_V1_TABLE)?.remove(seq)?;
        write_state_in(&write_txn, &state)?;
        write_txn.commit()?;
        self.sync.state = state;
        Ok(())
    }

    /// Exports what changed, at once (tests and the first export; the app
    /// works out the difference off the UI thread).
    pub fn export_now(&mut self) -> Result<Vec<u64>> {
        let snapshot = self.sync_snapshot()?;
        let Some(reader) = self.sync_reader() else {
            return Ok(Vec::new());
        };
        let export = reader.diff(&snapshot)?;
        self.commit_export(export)
    }
}

impl LibraryStore {
    /// Every register as changes, at the times they were set.
    fn registered_changes(&self) -> Result<Vec<Change>> {
        let Some(reader) = self.sync_reader() else {
            return Ok(Vec::new());
        };
        let registers = reader.registers()?;
        Ok(glyphst_sync::model::changes_of(registers.iter().flat_map(
            |(key, stored)| {
                stored
                    .fields
                    .iter()
                    .map(move |(field, register)| (key, field, register))
            },
        )))
    }

    /// Another copy of this library writes as this machine (its database
    /// was copied): this one becomes a new device and sends everything it
    /// has again under it, at the times it was set, so nothing is lost and
    /// nothing newer is overwritten.
    pub fn sync_fork(&mut self) -> Result<Vec<u64>> {
        let Some(database) = self.database.clone() else {
            return Ok(Vec::new());
        };
        let changes = self.registered_changes()?;
        let mut state = self.sync.state.clone();
        state.device = DeviceId::new();
        state.next_seq = 1;
        state.written.clear();
        let wall = wall_ms();
        let write_txn = database.begin_write()?;
        let mut seqs = Vec::new();
        {
            let mut outbox = write_txn.open_table(SYNC_OUTBOX_V1_TABLE)?;
            let old = outbox
                .iter()?
                .filter_map(|entry| entry.ok().map(|(seq, _)| seq.value()))
                .collect::<Vec<_>>();
            for seq in old {
                outbox.remove(seq)?;
            }
            for segment in journal::split_changes(changes) {
                let seq = state.next_seq;
                state.next_seq += 1;
                let bytes = journal::encode_segment(state.device, seq, wall, &segment);
                outbox.insert(seq, bytes.as_slice())?;
                seqs.push(seq);
            }
        }
        write_state_in(&write_txn, &state)?;
        write_txn.commit()?;
        self.sync.state = state;
        Ok(seqs)
    }

    /// What this machine's snapshot holds when it compacts its journal:
    /// its last segment written, the others' it read, and the merged
    /// state. `None` while segments wait to be written.
    pub fn sync_compaction(&self) -> Result<Option<(u64, glyphst_sync::Watermarks, Vec<Change>)>> {
        if !self.sync_outbox()?.is_empty() || self.sync.state.next_seq <= 1 {
            return Ok(None);
        }
        Ok(Some((
            self.sync.state.next_seq - 1,
            self.sync.state.marks.clone(),
            self.registered_changes()?,
        )))
    }
}
