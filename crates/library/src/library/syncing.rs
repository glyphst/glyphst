//! The library's side of syncing between machines: stable ids for its
//! records (`ids`), whatever each machine numbers them locally.

mod ids;

pub(super) use ids::{SYNC_IDS_V1_TABLE, SYNC_STATE_V1_TABLE, load_sync_ids};
pub use ids::{SyncIds, SyncKind};
