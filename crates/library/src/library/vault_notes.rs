//! Where each item's Markdown note is in the vault and what Glyphst last
//! wrote there, so a later write can tell the user's edits from its own.

use super::storage_helpers::{decode, encode};
use super::*;

pub(super) const LIBRARY_VAULT_NOTES_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_vault_notes_v1");
const VAULT_NOTES_STORAGE_VERSION: u8 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultNoteRecord {
    /// Relative to the vault, with `/` separators.
    pub relative_path: String,
    /// The tags written into the note's frontmatter, to see which the user
    /// changed there.
    pub written_tags: Vec<String>,
    pub frontmatter_sha256: [u8; 32],
    /// Each block Glyphst owns, by name, with the hash of what it wrote.
    pub blocks: Vec<(String, [u8; 32])>,
    /// The file's modification time after the last write or read (seconds).
    pub modified: u64,
    pub written_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredVaultNoteV1 {
    version: u8,
    note: VaultNoteRecord,
}

pub(super) fn ensure_vault_notes_table(write_txn: &redb::WriteTransaction) -> Result<()> {
    write_txn.open_table(LIBRARY_VAULT_NOTES_V1_TABLE)?;
    Ok(())
}

pub(super) fn load_vault_notes(
    database: &Database,
) -> Result<HashMap<DocumentId, VaultNoteRecord>> {
    let read_txn = database.begin_read()?;
    let table = read_txn.open_table(LIBRARY_VAULT_NOTES_V1_TABLE)?;
    let mut notes = HashMap::new();
    for entry in table.iter()? {
        let (id, value) = entry?;
        match decode::<StoredVaultNoteV1>(value.value()).and_then(|stored| {
            require_storage_version(stored.version, VAULT_NOTES_STORAGE_VERSION)?;
            Ok(stored.note)
        }) {
            Ok(note) => {
                notes.insert(DocumentId(id.value()), note);
            }
            Err(error) => eprintln!("failed to decode vault note: {error:#}"),
        }
    }
    Ok(notes)
}

impl LibraryStore {
    pub fn vault_note(&self, id: DocumentId) -> Option<&VaultNoteRecord> {
        self.vault_notes.get(&id)
    }

    pub fn vault_notes(&self) -> impl Iterator<Item = (DocumentId, &VaultNoteRecord)> {
        self.vault_notes.iter().map(|(id, note)| (*id, note))
    }

    /// Records the item's note, or forgets it with `None`.
    pub fn set_vault_note(&mut self, id: DocumentId, note: Option<VaultNoteRecord>) -> Result<()> {
        let previous = match &note {
            Some(note) => self.vault_notes.insert(id, note.clone()),
            None => self.vault_notes.remove(&id),
        };
        let result = (|| -> Result<()> {
            let Some(database) = &self.database else {
                return Ok(());
            };
            let write_txn = database.begin_write()?;
            {
                let mut table = write_txn.open_table(LIBRARY_VAULT_NOTES_V1_TABLE)?;
                match &note {
                    Some(note) => {
                        let encoded = encode(&StoredVaultNoteV1 {
                            version: VAULT_NOTES_STORAGE_VERSION,
                            note: note.clone(),
                        })?;
                        table.insert(id.0, encoded.as_slice())?;
                    }
                    None => {
                        table.remove(id.0)?;
                    }
                }
            }
            write_txn.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            match previous {
                Some(previous) => self.vault_notes.insert(id, previous),
                None => self.vault_notes.remove(&id),
            };
            return Err(error);
        }
        Ok(())
    }
}
