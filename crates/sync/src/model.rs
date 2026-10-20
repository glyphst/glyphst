//! What is synced: records of some kind under a key, each field a register
//! the latest change wins. A set is fields of its members (`tags/ml` =
//! true or false), and a removed record is the field [`DELETED`] = true,
//! which no later edit of another field undoes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hlc::Hlc;

/// The field that marks a record removed.
pub const DELETED: &str = "deleted";

pub type Fields = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RecordKey {
    pub kind: String,
    pub key: String,
}

impl RecordKey {
    pub fn new(kind: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            key: key.into(),
        }
    }
}

/// Fields of one record set at one time: a line of a journal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    #[serde(rename = "t")]
    pub hlc: Hlc,
    #[serde(rename = "k")]
    pub kind: String,
    #[serde(rename = "id")]
    pub key: String,
    #[serde(rename = "f")]
    pub fields: Fields,
}

impl Change {
    pub fn record(&self) -> RecordKey {
        RecordKey::new(self.kind.clone(), self.key.clone())
    }
}

/// A field's value and when it was set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Register {
    pub hlc: Hlc,
    pub value: Value,
}

/// Whether a value set at `incoming` replaces one set at `current`.
pub fn wins(incoming: Hlc, current: Option<Hlc>) -> bool {
    current.is_none_or(|current| incoming > current)
}

/// Every record's registers, merged: the same whatever order changes come
/// in, and however often.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    records: BTreeMap<RecordKey, BTreeMap<String, Register>>,
}

impl State {
    /// Merges `change`; returns the fields it set.
    pub fn apply(&mut self, change: &Change) -> Vec<String> {
        let record = self.records.entry(change.record()).or_default();
        let mut set = Vec::new();
        for (field, value) in &change.fields {
            if wins(change.hlc, record.get(field).map(|register| register.hlc)) {
                record.insert(
                    field.clone(),
                    Register {
                        hlc: change.hlc,
                        value: value.clone(),
                    },
                );
                set.push(field.clone());
            }
        }
        set
    }

    pub fn record(&self, key: &RecordKey) -> Option<&BTreeMap<String, Register>> {
        self.records.get(key)
    }

    pub fn records(&self) -> impl Iterator<Item = (&RecordKey, &BTreeMap<String, Register>)> {
        self.records.iter()
    }

    pub fn is_deleted(&self, key: &RecordKey) -> bool {
        self.record(key)
            .and_then(|fields| fields.get(DELETED))
            .is_some_and(|register| register.value == Value::Bool(true))
    }

    /// The state as changes, fields set at the same time on one line: what
    /// a snapshot holds.
    pub fn changes(&self) -> Vec<Change> {
        changes_of(self.records.iter().flat_map(|(key, fields)| {
            fields
                .iter()
                .map(move |(field, register)| (key, field, register))
        }))
    }
}

/// Registers as changes, one per record and time.
pub fn changes_of<'a>(
    registers: impl IntoIterator<Item = (&'a RecordKey, &'a String, &'a Register)>,
) -> Vec<Change> {
    let mut grouped: BTreeMap<(&RecordKey, Hlc), Fields> = BTreeMap::new();
    for (key, field, register) in registers {
        grouped
            .entry((key, register.hlc))
            .or_default()
            .insert(field.clone(), register.value.clone());
    }
    grouped
        .into_iter()
        .map(|((key, hlc), fields)| Change {
            hlc,
            kind: key.kind.clone(),
            key: key.key.clone(),
            fields,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::hlc::{Clock, DeviceId};

    /// A small deterministic shuffle (no randomness in tests).
    fn shuffle<T>(items: &mut [T], mut seed: u64) {
        for index in (1..items.len()).rev() {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            items.swap(index, (seed >> 33) as usize % (index + 1));
        }
    }

    fn change(clock: &mut Clock, wall: u64, key: &str, fields: &[(&str, Value)]) -> Change {
        Change {
            hlc: clock.now(wall),
            kind: "item".to_string(),
            key: key.to_string(),
            fields: fields
                .iter()
                .map(|(field, value)| (field.to_string(), value.clone()))
                .collect(),
        }
    }

    #[test]
    fn changes_from_three_devices_converge_in_any_order() {
        let mut clocks = [
            Clock::new(DeviceId::new(), None),
            Clock::new(DeviceId::new(), None),
            Clock::new(DeviceId::new(), None),
        ];
        let mut all = Vec::new();
        for step in 0..60u64 {
            let device = (step % 3) as usize;
            let key = format!("i{}", step % 4);
            let fields = match step % 5 {
                0 => vec![("title", json!(format!("t{step}")))],
                1 => vec![("tags/ml", json!(step % 2 == 0))],
                2 => vec![("year", json!(2000 + step)), ("title", json!("same"))],
                3 => vec![(DELETED, json!(step % 3 == 0))],
                _ => vec![("tags/nlp", json!(true))],
            };
            // The clocks disagree a little, as machines do.
            all.push(change(
                &mut clocks[device],
                1_000 + step * 7 - device as u64 * 30,
                &key,
                &fields,
            ));
        }

        let mut expected = State::default();
        for change in &all {
            expected.apply(change);
        }
        for seed in 0..20 {
            let mut order = all.clone();
            shuffle(&mut order, seed);
            let mut state = State::default();
            for change in &order {
                state.apply(change);
                // A change delivered twice changes nothing.
                state.apply(change);
            }
            assert_eq!(state, expected, "seed {seed}");
        }

        // A snapshot of the state rebuilds it.
        let mut rebuilt = State::default();
        for change in expected.changes() {
            rebuilt.apply(&change);
        }
        assert_eq!(rebuilt, expected);
    }

    #[test]
    fn a_removed_record_stays_removed_after_other_edits() {
        let mut here = Clock::new(DeviceId::new(), None);
        let mut there = Clock::new(DeviceId::new(), None);
        let removed = change(&mut here, 2_000, "a", &[(DELETED, json!(true))]);
        let edited = change(&mut there, 3_000, "a", &[("title", json!("later"))]);
        let mut state = State::default();
        state.apply(&edited);
        state.apply(&removed);
        assert!(state.is_deleted(&RecordKey::new("item", "a")));
    }
}
