//! Searching the library's text: an index of every item's pages in chunks
//! (keywords through SQLite FTS5, meaning through an embedder's vectors),
//! the thread that builds it, and the search over it.

pub mod chunk;
pub mod embed;
pub mod eval;
pub mod extract;
pub mod index;
pub mod indexer;
pub mod search;
pub mod store;
pub mod vectors;

pub use index::{IndexStatus, ItemText, ItemTextState, LibraryFile, Snapshot};
pub use indexer::Indexer;
pub use search::{ChunkLocation, Fusion, ItemHit, PageHit, Query, SearchRequest, SearchResults};
pub use store::{SCHEMA_VERSION, Store};

#[cfg(test)]
mod tests;
