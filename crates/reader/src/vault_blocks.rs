//! Blocks agents write in an item's vault note (`glyphst:agent-<name>`),
//! beside Glyphst's own. A block the user edited is left alone, and each
//! write is a change the assistant's log can undo.

use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use glyphst_core::DocumentId;
use glyphst_library::LibraryStore;

/// One block an agent wrote: what it held before and after (`None` when
/// there was none, or it was taken out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultBlockEdit {
    pub item: DocumentId,
    pub title: String,
    pub path: PathBuf,
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl VaultBlockEdit {
    pub fn describe(&self) -> String {
        match &self.after {
            Some(_) => format!(
                "Wrote the {} block in the note of “{}”",
                self.name, self.title
            ),
            None => format!(
                "Took the {} block out of the note of “{}”",
                self.name, self.title
            ),
        }
    }
}

/// Agents' blocks are `agent-` and a short lowercase name.
pub fn check_name(name: &str) -> Result<(), String> {
    let rest = name.strip_prefix("agent-").unwrap_or_default();
    if rest.is_empty()
        || name.len() > 40
        || !rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!(
            "{name} is not an agent block; use agent- and lowercase letters, digits or -, such as agent-claude"
        ));
    }
    Ok(())
}

/// Writes block `name` of item `id`'s note at `path` as `content` (`None`
/// takes it out), when it now reads `expected`. Keeps the note's record in
/// step, so the vault's sync sees Glyphst's own write.
pub fn write_block(
    library: &mut LibraryStore,
    id: DocumentId,
    path: &Path,
    name: &str,
    expected: Option<&str>,
    content: Option<&str>,
) -> Result<(), String> {
    let note =
        fs::read_to_string(path).map_err(|error| format!("could not read the note: {error}"))?;
    if glyphst_vault::block_text(&note, name).as_deref() != expected {
        return Err(format!(
            "the {name} block changed since; it was left as it is"
        ));
    }
    let text = glyphst_vault::replace_block(&note, name, content);
    let temporary = path.with_extension("md.glyphst-writing");
    fs::write(&temporary, &text)
        .and_then(|()| fs::rename(&temporary, path))
        .map_err(|error| format!("could not write the note: {error}"))?;
    let mut record = library
        .vault_note(id)
        .cloned()
        .ok_or("the item has no note")?;
    record.blocks.retain(|(written, _)| written != name);
    if let Some(content) = content {
        record
            .blocks
            .push((name.to_string(), glyphst_vault::block_hash(content)));
    }
    record.modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(record.modified, |duration| duration.as_secs());
    library
        .set_vault_note(id, Some(record))
        .map_err(|error| format!("could not save the note's record: {error:#}"))
}

/// The agent's write: refused when the user edited the block since an
/// agent last wrote it. `content` empty takes the block out.
pub fn agent_write(
    library: &mut LibraryStore,
    id: DocumentId,
    path: &Path,
    name: &str,
    content: &str,
) -> Result<VaultBlockEdit, String> {
    check_name(name)?;
    if content.contains("<!-- glyphst:") {
        return Err("the content cannot hold glyphst block markers".into());
    }
    let note =
        fs::read_to_string(path).map_err(|error| format!("could not read the note: {error}"))?;
    let before = glyphst_vault::block_text(&note, name);
    if let Some(current) = &before {
        let written = library
            .vault_note(id)
            .and_then(|record| record.blocks.iter().find(|(written, _)| written == name))
            .map(|(_, hash)| *hash);
        if written != Some(glyphst_vault::block_hash(current)) {
            return Err(format!(
                "the user edited the {name} block (or wrote it); it was left as it is"
            ));
        }
    }
    let content = content.trim();
    let after = (!content.is_empty()).then(|| content.to_string());
    write_block(library, id, path, name, before.as_deref(), after.as_deref())?;
    Ok(VaultBlockEdit {
        item: id,
        title: library
            .library_item(id)
            .map(|item| item.title.clone())
            .unwrap_or_default(),
        path: path.to_path_buf(),
        name: name.to_string(),
        before,
        after,
    })
}

#[cfg(test)]
mod tests {
    use glyphst_library::{LibraryImportOutcome, LibraryItemMetadata, VaultNoteRecord};

    use super::*;

    #[test]
    fn an_agent_writes_its_block_undoes_it_and_never_overwrites_the_user() {
        let dir = std::env::temp_dir().join(format!("glyphst-vault-blocks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("paper.pdf");
        fs::write(&pdf, b"%PDF-1.4 blocks").unwrap();
        let mut library = LibraryStore::memory();
        let LibraryImportOutcome::Added(id) = library
            .link_item(&pdf, LibraryItemMetadata::for_path(&pdf), None)
            .unwrap()
        else {
            panic!("expected an item");
        };
        let note = dir.join("paper.md");
        fs::write(&note, "# Paper\n\n## Notes\n\nMine.\n").unwrap();
        library
            .set_vault_note(
                id,
                Some(VaultNoteRecord {
                    relative_path: "paper.md".to_string(),
                    ..VaultNoteRecord::default()
                }),
            )
            .unwrap();

        assert!(check_name("summary").is_err());
        assert!(check_name("agent-Claude").is_err());
        let edit = agent_write(&mut library, id, &note, "agent-claude", "Cites BERT.").unwrap();
        assert_eq!(edit.before, None);
        let text = fs::read_to_string(&note).unwrap();
        assert!(text.contains("<!-- glyphst:agent-claude begin -->\nCites BERT.\n"));
        assert!(text.ends_with("## Notes\n\nMine.\n"));

        // Written again by the agent: fine. Undone: back as it was.
        let second = agent_write(
            &mut library,
            id,
            &note,
            "agent-claude",
            "Cites BERT and GPT-3.",
        )
        .unwrap();
        write_block(
            &mut library,
            id,
            &note,
            "agent-claude",
            second.after.as_deref(),
            second.before.as_deref(),
        )
        .unwrap();
        assert_eq!(
            glyphst_vault::block_text(&fs::read_to_string(&note).unwrap(), "agent-claude")
                .as_deref(),
            Some("Cites BERT.")
        );

        // The user edits it: the agent may not write over it.
        let edited = fs::read_to_string(&note)
            .unwrap()
            .replace("Cites BERT.", "Cites BERT (checked).");
        fs::write(&note, edited).unwrap();
        let error = agent_write(&mut library, id, &note, "agent-claude", "Again.").unwrap_err();
        assert!(error.contains("user edited"), "{error}");
        let _ = fs::remove_dir_all(dir);
    }
}
