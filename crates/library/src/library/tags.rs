//! Whose a tag on an item is, and the edits that respect it: a tag the AI
//! added (an `Applied` assignment) is the AI's to change, and a tag the
//! user added or kept (`Accepted`, or none) is never changed by any AI.
//! When a tag is renamed or deleted, the AI's assignments of it follow.

use super::*;

/// Who added a tag to an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagOwner {
    User,
    Ai,
}

/// What an AI's tag edit did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AiTagEdit {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// The user's tags it would have removed, kept.
    pub kept: Vec<String>,
}

impl LibraryStore {
    /// Who added `tag` to item `id`.
    pub fn tag_owner(&self, id: DocumentId, tag: LibraryTagId) -> TagOwner {
        let by_ai = self.ai_assignments(id).iter().any(|assignment| {
            assignment.target == AssignmentTarget::Tag(tag)
                && assignment.state == AssignmentState::Applied
        });
        if by_ai { TagOwner::Ai } else { TagOwner::User }
    }

    /// Sets an item's tags as an AI asks: tags it names that are missing
    /// are added as the AI's, the AI's tags it leaves out are removed, and
    /// the user's tags are kept whatever it says.
    pub fn set_item_tags_by_ai(
        &mut self,
        id: DocumentId,
        names: &[String],
        origin: AssignmentOrigin,
    ) -> Result<AiTagEdit> {
        let wanted = names
            .iter()
            .map(|name| normalize_tag_name(name))
            .filter(|name| !name.is_empty())
            .collect::<HashSet<_>>();
        let mut edit = AiTagEdit::default();
        let mut keep = Vec::new();
        let mut removed = Vec::new();
        for tag in self.tags_for_item(id) {
            let owner = self.tag_owner(id, tag.id);
            match (wanted.contains(&tag.normalized_name), owner) {
                (true, _) => keep.push(tag.id),
                (false, TagOwner::User) => {
                    keep.push(tag.id);
                    edit.kept.push(tag.name.clone());
                }
                (false, TagOwner::Ai) => {
                    removed.push(tag.id);
                    edit.removed.push(tag.name.clone());
                }
            }
        }
        let current = self
            .tags_for_item(id)
            .into_iter()
            .map(|tag| tag.normalized_name.clone())
            .collect::<HashSet<_>>();
        let mut added = Vec::new();
        for name in names {
            let normalized = normalize_tag_name(name);
            if normalized.is_empty()
                || current.contains(&normalized)
                || added.iter().any(|(_, known): &(LibraryTagId, String)| {
                    normalize_tag_name(known) == normalized
                })
            {
                continue;
            }
            let tag = self.ensure_library_tag(name)?;
            added.push((tag, name.trim().to_string()));
        }
        keep.extend(added.iter().map(|(tag, _)| *tag));
        self.replace_item_tags(id, &keep)?;
        let now = current_timestamp();
        let entries = self.ai_catalog.assignments.entry(id).or_default();
        entries.retain(|assignment| {
            !matches!(assignment.target, AssignmentTarget::Tag(tag) if removed.contains(&tag))
        });
        for (tag, name) in added {
            entries.retain(|assignment| assignment.target != AssignmentTarget::Tag(tag));
            entries.push(AiAssignment {
                target: AssignmentTarget::Tag(tag),
                label: name.clone(),
                origin: origin.clone(),
                state: AssignmentState::Applied,
                at: now,
            });
            edit.added.push(name);
        }
        if entries.is_empty() {
            self.ai_catalog.assignments.remove(&id);
        }
        self.write_ai_rows(&[id])?;
        Ok(edit)
    }

    /// Points the AI's assignments of tag `id` at `target` (a rename,
    /// or a merge into another tag), before the catalog changes, so its
    /// clean-up does not drop them. Returns the items to write.
    pub(super) fn retarget_tag_assignments(
        &mut self,
        id: LibraryTagId,
        target: LibraryTagId,
        label: &str,
    ) -> Vec<DocumentId> {
        let mut touched = Vec::new();
        for (item, entries) in &mut self.ai_catalog.assignments {
            if !entries
                .iter()
                .any(|assignment| assignment.target == AssignmentTarget::Tag(id))
            {
                continue;
            }
            for assignment in entries.iter_mut() {
                if assignment.target == AssignmentTarget::Tag(id) {
                    assignment.target = AssignmentTarget::Tag(target);
                    assignment.label = label.to_string();
                }
            }
            // An item that had both keeps one assignment of the tag.
            let mut seen = Vec::new();
            entries.retain(|assignment| {
                if seen.contains(&assignment.target) {
                    return false;
                }
                seen.push(assignment.target.clone());
                true
            });
            touched.push(*item);
        }
        touched
    }

    /// After a tag was deleted, the AI's assignments of it go too.
    pub(super) fn forget_tag_assignments(&mut self, id: LibraryTagId) -> Result<()> {
        let mut touched = Vec::new();
        for (item, entries) in &mut self.ai_catalog.assignments {
            let before = entries.len();
            entries.retain(|assignment| assignment.target != AssignmentTarget::Tag(id));
            if entries.len() != before {
                touched.push(*item);
            }
        }
        self.ai_catalog
            .assignments
            .retain(|_, entries| !entries.is_empty());
        self.write_ai_rows(&touched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (LibraryStore, DocumentId, PathBuf) {
        let dir = std::env::temp_dir().join(format!("glyphst-tags-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("paper.pdf");
        fs::write(&path, b"%PDF-1.4 tags").unwrap();
        let mut store = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
        let LibraryImportOutcome::Added(id) = store
            .link_item(&path, LibraryItemMetadata::for_path(&path), None)
            .unwrap()
        else {
            panic!("expected an item");
        };
        (store, id, dir)
    }

    fn run(store: &mut LibraryStore, id: DocumentId, tags: &[&str]) {
        let run = store.next_enrichment_run();
        store
            .apply_enrichment(
                id,
                EnrichmentApply {
                    run,
                    tags: tags.iter().map(|tag| tag.to_string()).collect(),
                    create_tags: true,
                    collections: Vec::new(),
                    suggested_collection: None,
                    metadata: None,
                    enrichment: None,
                },
            )
            .unwrap();
    }

    fn names(store: &LibraryStore, id: DocumentId) -> Vec<(String, TagOwner)> {
        let mut names = store
            .tags_for_item(id)
            .into_iter()
            .map(|tag| (tag.normalized_name.clone(), store.tag_owner(id, tag.id)))
            .collect::<Vec<_>>();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        names
    }

    #[test]
    fn the_ai_changes_its_own_tags_and_never_the_users() {
        let (mut store, id, dir) = store("owners");
        store.set_item_tag_names(id, &["mine".to_string()]).unwrap();
        run(&mut store, id, &["alpha", "beta"]);
        assert_eq!(
            names(&store, id),
            vec![
                ("alpha".to_string(), TagOwner::Ai),
                ("beta".to_string(), TagOwner::Ai),
                ("mine".to_string(), TagOwner::User)
            ]
        );
        // A later run that no longer suggests alpha takes it off.
        run(&mut store, id, &["beta", "gamma"]);
        let tags = names(&store, id)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(tags, vec!["beta", "gamma", "mine"]);

        // Kept as the user's, the AI leaves it alone from then on.
        let gamma = tag_named(&store, "gamma");
        store
            .accept_assignment(id, &AssignmentTarget::Tag(gamma))
            .unwrap();
        assert_eq!(store.tag_owner(id, gamma), TagOwner::User);
        run(&mut store, id, &["beta"]);
        let tags = names(&store, id)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(tags, vec!["beta", "gamma", "mine"]);

        // Removed by the user, an AI tag never comes back.
        let beta = tag_named(&store, "beta");
        store
            .reject_assignment(id, &AssignmentTarget::Tag(beta))
            .unwrap();
        run(&mut store, id, &["beta", "delta"]);
        let tags = names(&store, id)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(tags, vec!["delta", "gamma", "mine"]);
        fs::remove_dir_all(dir).unwrap();
    }

    fn tag_named(store: &LibraryStore, name: &str) -> LibraryTagId {
        store
            .library_tags()
            .iter()
            .find(|tag| tag.normalized_name == name)
            .unwrap()
            .id
    }

    #[test]
    fn an_assistant_cannot_remove_the_users_tags() {
        let (mut store, id, dir) = store("assistant");
        store.set_item_tag_names(id, &["mine".to_string()]).unwrap();
        run(&mut store, id, &["alpha"]);
        let edit = store
            .set_item_tags_by_ai(
                id,
                &["new".to_string()],
                AssignmentOrigin::Agent {
                    name: "claude".to_string(),
                },
            )
            .unwrap();
        assert_eq!(edit.added, vec!["new".to_string()]);
        assert_eq!(edit.removed, vec!["alpha".to_string()]);
        assert_eq!(edit.kept, vec!["mine".to_string()]);
        assert_eq!(
            names(&store, id),
            vec![
                ("mine".to_string(), TagOwner::User),
                ("new".to_string(), TagOwner::Ai)
            ]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn renaming_and_deleting_a_tag_carry_the_ais_assignments() {
        let (mut store, id, dir) = store("rename");
        run(&mut store, id, &["nlp"]);
        let nlp = tag_named(&store, "nlp");
        let other = store.ensure_library_tag("language").unwrap();
        store
            .set_tag_color(nlp, Some(HighlightColor::Blue))
            .unwrap();
        // Into an existing tag: one tag, still the AI's on the item.
        let merged = store.rename_library_tag(nlp, "Language").unwrap();
        assert_eq!(merged, other);
        assert_eq!(store.tag_owner(id, other), TagOwner::Ai);
        assert_eq!(store.ai_assignments(id)[0].label, "Language");
        assert!(store.library_tags().iter().all(|tag| tag.id != nlp));

        store.delete_library_tag(other).unwrap();
        assert!(store.tags_for_item(id).is_empty());
        assert!(store.ai_assignments(id).is_empty());
        assert!(store.tag_colors().iter().all(|color| color.tag_id != other));
        fs::remove_dir_all(dir).unwrap();
    }
}
