//! Syncing a library between machines through files: each machine writes
//! its changes to a journal of its own in the library folder (`journal`),
//! carried by a sync tool or mirrored to a server, and merges the others'
//! field by field, the latest change winning (`model`, ordered by `hlc`).
//! What the records are is the library's.

pub mod hlc;
pub mod journal;
pub mod model;

pub use hlc::{Clock, DeviceId, Hlc, Observed, wall_ms};
pub use journal::{Batch, DeviceInfo, DeviceListing, Journal, Watermarks};
pub use model::{Change, DELETED, Fields, RecordKey, Register, State, wins};
