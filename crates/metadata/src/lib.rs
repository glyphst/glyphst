//! Recognising library items: the identifiers a document prints, what
//! public catalogues know about it, whether the library already has it,
//! and how that becomes the item's metadata.

pub mod dedupe;
pub mod identifiers;
pub mod lookup;
pub mod merge;

use anyhow::Result;

pub use dedupe::{DuplicateReason, find_duplicate};
pub use identifiers::Identifiers;
pub use lookup::{MetadataClient, MetadataEndpoints};
pub use merge::{WorkMetadata, embedded_work, merge_work};

/// What recognition found for one document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recognized {
    pub identifiers: Identifiers,
    /// A catalogue's answer, when one knew the work.
    pub work: Option<WorkMetadata>,
    /// What the file's info entries say.
    pub embedded: WorkMetadata,
}

/// Finds the document's identifiers in `pages` (the text of its first
/// pages), `info` and `file_stem`, and looks them up with `client` (no
/// lookup without one). Fails when the catalogues could not be reached, so
/// the caller can try again later; a work no catalogue knows is `None`.
pub fn recognize(
    client: Option<&MetadataClient>,
    pages: &[String],
    info: &[(&str, String)],
    file_stem: &str,
) -> Result<Recognized> {
    let info_values = info
        .iter()
        .filter(|(key, _)| matches!(*key, "subject" | "keywords" | "title"))
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    let identifiers = identifiers::extract(pages, file_stem, &info_values);
    let embedded = embedded_work(info, file_stem);
    let work = match client {
        Some(client) if !identifiers.is_empty() => client.lookup(&identifiers)?,
        _ => None,
    };
    Ok(Recognized {
        identifiers,
        work,
        embedded,
    })
}
