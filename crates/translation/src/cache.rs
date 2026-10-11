use std::{
    collections::HashMap,
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

use super::*;

const CACHE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("translations");
const MAX_CACHE_ENTRIES: usize = 10_000;

pub(super) struct TranslationCache {
    pub(super) database: Option<Database>,
    pub(super) memory: HashMap<String, CachedTranslation>,
}

impl TranslationCache {
    pub(super) fn memory() -> Self {
        Self {
            database: None,
            memory: HashMap::new(),
        }
    }

    pub(super) fn open() -> Result<Self> {
        let path = glyphst_core::paths::state_dir().join("translations-v2.redb");
        Self::open_at(&path)
    }

    pub(super) fn open_at(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            create_private_cache_directory(parent)?;
        }
        if !path.exists() {
            create_private_cache_file(path)?;
        }
        let (database, replaced) = match Database::create(path) {
            Ok(database) => (database, None),
            Err(redb::DatabaseError::UpgradeRequired(version)) => {
                // The cache is disposable, but keep the old file rather than
                // deleting it; a fresh cache replaces it.
                let aside = path.with_extension(format!("redb{version}-format"));
                fs::rename(path, &aside)
                    .with_context(|| format!("failed to move {} aside", path.display()))?;
                create_private_cache_file(path)?;
                let database = Database::create(path)
                    .with_context(|| format!("failed to create {}", path.display()))?;
                (database, Some(aside))
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to open {}", path.display()));
            }
        };
        if let Some(aside) = replaced {
            eprintln!(
                "translation cache used an older database format; kept it at {}",
                aside.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to restrict {}", path.display()))?;
        }
        let write_txn = database.begin_write()?;
        write_txn.open_table(CACHE_TABLE)?;
        write_txn.commit()?;
        Ok(Self {
            database: Some(database),
            ..Self::memory()
        })
    }

    pub(super) fn clear(&mut self) -> Result<()> {
        if let Some(database) = &self.database {
            let write_txn = database.begin_write()?;
            {
                let mut table = write_txn.open_table(CACHE_TABLE)?;
                table.retain(|_, _| false)?;
            }
            write_txn.commit()?;
        }
        self.memory.clear();
        Ok(())
    }

    pub(super) fn get(&mut self, key: &str) -> Option<CachedTranslation> {
        if let Some(value) = self.memory.get(key) {
            return Some(CachedTranslation {
                translations: value.translations.clone(),
                detected_source_language: value.detected_source_language.clone(),
                updated_at: value.updated_at,
            });
        }
        let database = self.database.as_ref()?;
        let read_txn = database.begin_read().ok()?;
        let table = read_txn.open_table(CACHE_TABLE).ok()?;
        let value = table.get(key).ok()??;
        let cached = serde_json::from_slice::<CachedTranslation>(value.value()).ok()?;
        self.memory.insert(
            key.to_string(),
            CachedTranslation {
                translations: cached.translations.clone(),
                detected_source_language: cached.detected_source_language.clone(),
                updated_at: cached.updated_at,
            },
        );
        self.prune_memory_cache();
        Some(cached)
    }

    pub(super) fn insert(&mut self, key: &str, value: CachedTranslation) -> Result<()> {
        self.insert_many([(key.to_string(), value)])
    }

    pub(super) fn insert_many(
        &mut self,
        entries: impl IntoIterator<Item = (String, CachedTranslation)>,
    ) -> Result<()> {
        let entries = entries.into_iter().collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
        }
        for (key, value) in &entries {
            self.memory.insert(
                key.clone(),
                CachedTranslation {
                    translations: value.translations.clone(),
                    detected_source_language: value.detected_source_language.clone(),
                    updated_at: value.updated_at,
                },
            );
        }
        self.prune_memory_cache();
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(CACHE_TABLE)?;
            for (key, value) in entries {
                let encoded = serde_json::to_vec(&value)?;
                table.insert(key.as_str(), encoded.as_slice())?;
            }
            if table.len()? > MAX_CACHE_ENTRIES as u64 {
                prune_cache_table(&mut table)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub(super) fn prune_memory_cache(&mut self) {
        if self.memory.len() <= MAX_CACHE_ENTRIES {
            return;
        }
        let mut entries = self
            .memory
            .iter()
            .map(|(key, value)| (key.clone(), value.updated_at))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(_, updated_at)| *updated_at);
        for (key, _) in entries
            .into_iter()
            .take(self.memory.len() - MAX_CACHE_ENTRIES)
        {
            self.memory.remove(&key);
        }
    }
}

pub(super) fn create_private_cache_directory(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to restrict {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }

    Ok(())
}

pub(super) fn create_private_cache_file(path: &std::path::Path) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    Ok(())
}

pub(super) fn prune_cache_table(table: &mut redb::Table<'_, &str, &[u8]>) -> Result<()> {
    let mut entries = table
        .iter()?
        .filter_map(|entry| {
            let (key, value) = entry.ok()?;
            let cached = serde_json::from_slice::<CachedTranslation>(value.value()).ok()?;
            Some((key.value().to_string(), cached.updated_at))
        })
        .collect::<Vec<_>>();
    if entries.len() <= MAX_CACHE_ENTRIES {
        return Ok(());
    }
    entries.sort_by_key(|(_, updated_at)| *updated_at);
    let remove_count = entries.len() - MAX_CACHE_ENTRIES;
    for (key, _) in entries.into_iter().take(remove_count) {
        table.remove(key.as_str())?;
    }
    Ok(())
}

pub(super) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
