use super::storage_helpers::{ensure_managed_library_directories, restrict_library_file};
use super::*;

impl LibraryStore {
    pub fn ai_sessions(&self) -> Vec<AiSession> {
        self.library.ai_sessions.clone()
    }

    /// The latest conversation about item `id`, to continue it.
    pub fn latest_item_session(&self, id: DocumentId) -> Option<&AiSession> {
        self.library
            .ai_sessions
            .iter()
            .filter(|session| session.item == Some(id))
            .max_by_key(|session| session.updated)
    }

    pub fn next_ai_session_id(&mut self) -> u64 {
        let id = self.library.next_ai_session_id.max(1);
        self.library.next_ai_session_id = id + 1;
        id
    }
    /// Removes a saved chat and the pictures only it used.
    pub fn delete_ai_session(&mut self, id: u64) -> Result<()> {
        if !self
            .library
            .ai_sessions
            .iter()
            .any(|session| session.id == id)
        {
            return Ok(());
        }
        if let Some(database) = &self.database {
            let write_txn = database.begin_write()?;
            {
                let mut table = write_txn.open_table(AI_SESSIONS_V6_TABLE)?;
                table.remove(id)?;
            }
            write_txn.commit()?;
        }
        self.library.ai_sessions.retain(|session| session.id != id);
        if let Err(error) = self.garbage_collect_ai_images() {
            eprintln!("failed to prune unused AI images: {error:#}");
        }
        Ok(())
    }

    pub fn save_ai_session(&mut self, mut session: AiSession) -> Result<()> {
        session.title = session.title.trim().to_string();
        if session.title.is_empty() {
            session.title = "New Chat".to_string();
        }
        let session_id = session.id;

        let mut library = self.library.clone();
        if let Some(session_ix) = library
            .ai_sessions
            .iter()
            .position(|existing| existing.id == session_id)
        {
            library.ai_sessions[session_ix] = session;
        } else {
            library.ai_sessions.push(session);
        }
        library.sanitize();

        let session = library
            .ai_sessions
            .iter()
            .find(|existing| existing.id == session_id)
            .ok_or_else(|| anyhow!("saved AI session is invalid"))?;
        let retained_ids = library
            .ai_sessions
            .iter()
            .map(|existing| existing.id)
            .collect::<HashSet<_>>();
        let removed_ids = self
            .library
            .ai_sessions
            .iter()
            .map(|existing| existing.id)
            .filter(|id| !retained_ids.contains(id))
            .collect::<Vec<_>>();

        self.write_ai_session_changes(session, &removed_ids)?;
        self.library = library;
        if let Err(error) = self.garbage_collect_ai_images() {
            eprintln!("failed to prune unused AI images: {error:#}");
        }
        Ok(())
    }

    pub fn install_ai_image(
        &mut self,
        name: Option<&str>,
        media_type: &str,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) -> Result<AiSessionImage> {
        let extension = ai_image_extension(media_type)?;
        if width == 0 || height == 0 || bytes.is_empty() {
            return Err(anyhow!("AI image is empty or has invalid dimensions"));
        }
        let asset_id = glyphst_core::lower_hex(Sha256::digest(bytes));
        let image = AiSessionImage {
            asset_id: asset_id.clone(),
            name: name
                .and_then(|name| Path::new(name).file_name())
                .and_then(|name| name.to_str())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            media_type: media_type.to_string(),
            width,
            height,
            byte_len: bytes.len() as u64,
        };

        let Some(root) = self.managed_library_root.as_deref() else {
            self.ai_image_memory.insert(asset_id, bytes.to_vec());
            return Ok(image);
        };
        ensure_managed_library_directories(root)?;
        let final_path = root
            .join("ai-images")
            .join(format!("{}.{}", image.asset_id, extension));
        if final_path.exists() {
            validate_ai_image_file(&final_path, &image, bytes)?;
            return Ok(image);
        }

        let staging_path = root.join(".staging").join(format!(
            "ai-image-{}.{}",
            Uuid::new_v4().simple(),
            extension
        ));
        {
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staging_path)
                .with_context(|| format!("failed to create {}", staging_path.display()))?;
            output
                .write_all(bytes)
                .with_context(|| format!("failed to write {}", staging_path.display()))?;
            output
                .sync_all()
                .with_context(|| format!("failed to sync {}", staging_path.display()))?;
        }
        restrict_library_file(&staging_path)?;
        match fs::hard_link(&staging_path, &final_path) {
            Ok(()) => {}
            Err(_) if final_path.is_file() => {
                validate_ai_image_file(&final_path, &image, bytes)?;
            }
            Err(error) => {
                let _ = fs::remove_file(&staging_path);
                return Err(error).with_context(|| {
                    format!("failed to install AI image at {}", final_path.display())
                });
            }
        }
        fs::remove_file(&staging_path)
            .with_context(|| format!("failed to remove {}", staging_path.display()))?;
        Ok(image)
    }

    pub fn load_ai_image(&self, image: &AiSessionImage) -> Result<Vec<u8>> {
        validate_ai_image_reference(image)?;
        if let Some(bytes) = self.ai_image_memory.get(&image.asset_id) {
            validate_ai_image_bytes(image, bytes)?;
            return Ok(bytes.clone());
        }
        let root = self
            .managed_library_root
            .as_deref()
            .ok_or_else(|| anyhow!("managed AI image storage is unavailable"))?;
        let extension = ai_image_extension(&image.media_type)?;
        let path = root
            .join("ai-images")
            .join(format!("{}.{}", image.asset_id, extension));
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(anyhow!("AI image asset is not a regular file"));
        }
        let bytes =
            fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        validate_ai_image_bytes(image, &bytes)?;
        Ok(bytes)
    }

    fn garbage_collect_ai_images(&mut self) -> Result<()> {
        let referenced = self
            .library
            .ai_sessions
            .iter()
            .flat_map(|session| session.messages.iter().chain(&session.queued_messages))
            .flat_map(ai_message_image_ids)
            .collect::<HashSet<_>>();
        self.ai_image_memory
            .retain(|asset_id, _| referenced.contains(asset_id.as_str()));

        let Some(root) = self.managed_library_root.as_deref() else {
            return Ok(());
        };
        let directory = root.join("ai-images");
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to read {}", directory.display()))?
        {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                continue;
            }
            let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some((asset_id, extension)) = file_name.rsplit_once('.') else {
                continue;
            };
            if asset_id.len() != 64
                || !asset_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !matches!(extension, "png" | "jpg" | "gif" | "webp")
                || referenced.contains(asset_id)
            {
                continue;
            }
            fs::remove_file(entry.path()).with_context(|| {
                format!(
                    "failed to remove unused AI image {}",
                    entry.path().display()
                )
            })?;
        }
        Ok(())
    }
}

fn ai_image_extension(media_type: &str) -> Result<&'static str> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Ok("png"),
        "image/jpeg" | "image/jpg" => Ok("jpg"),
        "image/gif" => Ok("gif"),
        "image/webp" => Ok("webp"),
        _ => Err(anyhow!("unsupported AI image media type: {media_type}")),
    }
}

fn validate_ai_image_reference(image: &AiSessionImage) -> Result<()> {
    ai_image_extension(&image.media_type)?;
    if image.asset_id.len() != 64
        || !image
            .asset_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || image.width == 0
        || image.height == 0
        || image.byte_len == 0
    {
        return Err(anyhow!("invalid managed AI image reference"));
    }
    Ok(())
}

fn validate_ai_image_bytes(image: &AiSessionImage, bytes: &[u8]) -> Result<()> {
    validate_ai_image_reference(image)?;
    if bytes.len() as u64 != image.byte_len {
        return Err(anyhow!(
            "managed AI image length does not match its reference"
        ));
    }
    let actual = glyphst_core::lower_hex(Sha256::digest(bytes));
    if actual != image.asset_id {
        return Err(anyhow!(
            "managed AI image hash does not match its reference"
        ));
    }
    Ok(())
}

fn validate_ai_image_file(path: &Path, image: &AiSessionImage, expected: &[u8]) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(anyhow!("managed AI image path is not a regular file"));
    }
    let existing = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    validate_ai_image_bytes(image, &existing)?;
    if existing != expected {
        return Err(anyhow!("managed AI image content collision"));
    }
    Ok(())
}

fn ai_message_image_ids(message: &AiSessionMessage) -> Vec<&str> {
    let mut ids = message
        .images
        .iter()
        .map(|image| image.asset_id.as_str())
        .collect::<Vec<_>>();
    for part in &message.agent_parts {
        let AiSessionAgentPart::ToolResult { content, .. } = part else {
            continue;
        };
        ids.extend(content.iter().filter_map(|part| match part {
            AiSessionToolResultPart::Image(image) => Some(image.asset_id.as_str()),
            AiSessionToolResultPart::Text(_) => None,
        }));
    }
    ids
}
