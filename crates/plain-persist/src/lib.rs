#![deny(clippy::todo, clippy::unimplemented)]

//! F8 Single-file SQLite WAL persistence with full Zstd-compressed finite-world baseline
//! (`world_baseline`), incremental `dirty_chunks` deltas, dual versioning (`WorldGenVersion` +
//! `SchemaVersion`), and deterministic BLAKE3 full-state verification hashing.
//!
//! Stage 1 strictly creates only the tables actively used in Stage 1 (`world_meta`,
//! `world_baseline`, `dirty_chunks`) — zero empty placeholder tables for future stages.

pub mod hash;
pub mod schema;
pub mod store;

pub use hash::{hash_to_hex, world_state_hash};
pub use schema::{ChunkBlobV1, VersionedChunkBlob, VersionedWorldMeta, WorldMetaV1, WorldMetaV2};
pub use store::{PersistError, SaveStats, WorldSaveStore};
