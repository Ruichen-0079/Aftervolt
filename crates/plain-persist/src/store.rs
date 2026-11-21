use std::path::Path;
use std::time::Instant;

use plain_contracts::config::CURRENT_SCHEMA_VERSION;
use plain_contracts::{FluidKind, CHUNK_SIZE};
use plain_world::WorldState;
use rayon::prelude::*;
use rusqlite::{params, Connection};
use thiserror::Error;

use crate::schema::{ChunkBlobV3, VersionedChunkBlob, VersionedWorldMeta, WorldMetaV4};

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("compression error: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt or missing save metadata key: {0}")]
    MissingMeta(&'static str),
    #[error("WAL checkpoint blocked by an active SQLite connection")]
    WalCheckpointBusy,
    #[error("legacy water debt exceeds the available water in the save")]
    InvalidLegacyWater,
    #[error("chunk count mismatch: expected {expected}, loaded {actual}")]
    ChunkCountMismatch { expected: usize, actual: usize },
}

#[derive(Debug, Clone, Default)]
pub struct SaveStats {
    pub baseline_chunks_written: usize,
    pub dirty_chunks_written: usize,
    pub compressed_bytes_written: usize,
    pub elapsed_ms: f64,
    pub file_size_bytes: u64,
}

/// SQLite world store (F8). Runtime writes use WAL and may create `-wal` and
/// `-shm` companions. Closing the last connection checkpoints to a standalone database.
///
/// Design invariants:
/// 1. At world genesis, `save_full_baseline` persists every chunk (L0+L1) compressed with Zstd
///    into `world_baseline`. Loading an older save NEVER re-executes `plain-worldgen`, eliminating
///    worldgen-upgrade terrain cliff seams permanently.
/// 2. Runtime saves (`save_incremental`) only write `world_meta` + chunks where `dirty_for_save == true`
///    inside a single atomic SQLite WAL transaction.
pub struct WorldSaveStore {
    conn: Connection,
    zstd_level: i32,
}

impl WorldSaveStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, PersistError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        Self::init_tables(&conn)?;
        Ok(Self {
            conn,
            zstd_level: 3,
        })
    }

    /// Merge committed WAL frames into the database and truncate the log.
    pub fn checkpoint_truncate(&mut self) -> Result<(), PersistError> {
        let (busy, _, _): (i32, i32, i32) =
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
        if busy != 0 {
            return Err(PersistError::WalCheckpointBusy);
        }
        Ok(())
    }

    /// Checkpoint and release the connection. SQLite removes the companion files
    /// when the last connection closes. Close all connections before export.
    pub fn close(mut self) -> Result<(), PersistError> {
        self.checkpoint_truncate()?;
        self.conn.close().map_err(|(_, error)| error.into())
    }

    fn init_tables(conn: &Connection) -> Result<(), PersistError> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS world_meta (
                key TEXT PRIMARY KEY,
                value BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS world_baseline (
                cx INTEGER NOT NULL,
                cy INTEGER NOT NULL,
                data BLOB NOT NULL,
                PRIMARY KEY (cx, cy)
            );
            CREATE TABLE IF NOT EXISTS dirty_chunks (
                cx INTEGER NOT NULL,
                cy INTEGER NOT NULL,
                last_macro_step INTEGER NOT NULL,
                data BLOB NOT NULL,
                PRIMARY KEY (cx, cy)
            );
            ",
        )?;
        Ok(())
    }

    /// Write full initial world baseline snapshot + metadata atomically (called once at genesis).
    pub fn save_full_baseline(
        &mut self,
        world: &mut WorldState,
        file_path_for_stat: Option<&Path>,
    ) -> Result<SaveStats, PersistError> {
        let t0 = Instant::now();
        let chunks_x = world.config.dimensions.chunks_x as usize;
        let zstd_level = self.zstd_level;

        // Parallel serialize & Zstd-compress every chunk before opening the SQLite write transaction.
        let compressed_chunks: Result<Vec<(i32, i32, Vec<u8>)>, PersistError> = world
            .l0_chunks
            .par_iter()
            .zip(world.l1_chunks.par_iter())
            .enumerate()
            .map(|(ci, (l0, l1))| {
                let cx = (ci % chunks_x) as i32;
                let cy = (ci / chunks_x) as i32;
                let blob = VersionedChunkBlob::V3(ChunkBlobV3 {
                    l0: l0.clone(),
                    l1: l1.clone(),
                });
                let raw = postcard::to_allocvec(&blob)?;
                let comp = zstd::bulk::compress(&raw, zstd_level)?;
                Ok((cx, cy, comp))
            })
            .collect();
        let compressed_chunks = compressed_chunks?;
        let meta_bytes = Self::encode_meta(world, zstd_level)?;

        let mut total_bytes = meta_bytes.len();
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM dirty_chunks", [])?;
        tx.execute("DELETE FROM world_baseline", [])?;
        tx.execute(
            "INSERT OR REPLACE INTO world_meta (key, value) VALUES (?1, ?2)",
            params!["world_meta_v1", meta_bytes],
        )?;
        {
            let mut stmt =
                tx.prepare("INSERT INTO world_baseline (cx, cy, data) VALUES (?1, ?2, ?3)")?;
            for (cx, cy, data) in &compressed_chunks {
                total_bytes += data.len();
                stmt.execute(params![cx, cy, data])?;
            }
        }
        tx.commit()?;
        // Checkpoint WAL into the main SQLite file after baseline creation for single-file portability & accurate size stats
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");

        for ch in &mut world.l0_chunks {
            ch.sleep.dirty_for_save = false;
        }

        let file_size_bytes = file_path_for_stat
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);

        Ok(SaveStats {
            baseline_chunks_written: compressed_chunks.len(),
            dirty_chunks_written: 0,
            compressed_bytes_written: total_bytes,
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            file_size_bytes,
        })
    }

    /// Incrementally save updated `world_meta` and only chunks flagged `dirty_for_save == true`.
    pub fn save_incremental(
        &mut self,
        world: &mut WorldState,
        file_path_for_stat: Option<&Path>,
    ) -> Result<SaveStats, PersistError> {
        let t0 = Instant::now();
        let chunks_x = world.config.dimensions.chunks_x as usize;
        let zstd_level = self.zstd_level;

        let dirty_indices: Vec<usize> = world
            .l0_chunks
            .iter()
            .enumerate()
            .filter_map(|(ci, ch)| {
                if ch.sleep.dirty_for_save {
                    Some(ci)
                } else {
                    None
                }
            })
            .collect();

        let compressed_dirty: Result<Vec<(i32, i32, i64, Vec<u8>)>, PersistError> = dirty_indices
            .par_iter()
            .map(|&ci| {
                let cx = (ci % chunks_x) as i32;
                let cy = (ci / chunks_x) as i32;
                let l0 = &world.l0_chunks[ci];
                let l1 = &world.l1_chunks[ci];
                let step = l0.last_realized_macro_step as i64;
                let blob = VersionedChunkBlob::V3(ChunkBlobV3 {
                    l0: l0.clone(),
                    l1: l1.clone(),
                });
                let raw = postcard::to_allocvec(&blob)?;
                let comp = zstd::bulk::compress(&raw, zstd_level)?;
                Ok((cx, cy, step, comp))
            })
            .collect();
        let compressed_dirty = compressed_dirty?;
        let meta_bytes = Self::encode_meta(world, zstd_level)?;

        let mut total_bytes = meta_bytes.len();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO world_meta (key, value) VALUES (?1, ?2)",
            params!["world_meta_v1", meta_bytes],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO dirty_chunks (cx, cy, last_macro_step, data) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (cx, cy, step, data) in &compressed_dirty {
                total_bytes += data.len();
                stmt.execute(params![cx, cy, step, data])?;
            }
        }
        tx.commit()?;

        for &ci in &dirty_indices {
            world.l0_chunks[ci].sleep.dirty_for_save = false;
        }

        let file_size_bytes = file_path_for_stat
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);

        Ok(SaveStats {
            baseline_chunks_written: 0,
            dirty_chunks_written: compressed_dirty.len(),
            compressed_bytes_written: total_bytes,
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            file_size_bytes,
        })
    }

    /// Reconstruct the complete `WorldState` strictly from SQLite (`world_baseline` + `dirty_chunks`),
    /// completely independent of any `plain-worldgen` generator version.
    pub fn load_world(&self) -> Result<WorldState, PersistError> {
        let meta_comp: Vec<u8> = self
            .conn
            .query_row(
                "SELECT value FROM world_meta WHERE key = ?1",
                params!["world_meta_v1"],
                |row| row.get(0),
            )
            .map_err(|_| PersistError::MissingMeta("world_meta_v1"))?;
        let meta_raw = zstd::stream::decode_all(&meta_comp[..])?;
        let versioned_meta: VersionedWorldMeta = postcard::from_bytes(&meta_raw)?;
        let legacy_water = matches!(versioned_meta, VersionedWorldMeta::V1(_));
        let legacy_history = matches!(
            versioned_meta,
            VersionedWorldMeta::V1(_) | VersionedWorldMeta::V2(_)
        );
        let meta = versioned_meta.migrate_to_current();

        let chunks_x = meta.config.dimensions.chunks_x as usize;
        let total_chunks = meta.config.dimensions.total_chunks();

        // Read all baseline chunks, then overlay dirty_chunks via SQL COALESCE(dirty, baseline)
        let mut raw_rows: Vec<(usize, Vec<u8>)> = Vec::with_capacity(total_chunks);
        {
            let mut stmt = self.conn.prepare(
                "
                SELECT b.cx, b.cy, COALESCE(d.data, b.data)
                FROM world_baseline b
                LEFT JOIN dirty_chunks d ON b.cx = d.cx AND b.cy = d.cy
                ORDER BY b.cy ASC, b.cx ASC
                ",
            )?;
            let iter = stmt.query_map([], |row| {
                let cx: i32 = row.get(0)?;
                let cy: i32 = row.get(1)?;
                let data: Vec<u8> = row.get(2)?;
                Ok(((cy as usize) * chunks_x + (cx as usize), data))
            })?;
            for item in iter {
                raw_rows.push(item?);
            }
        }

        if raw_rows.len() != total_chunks {
            return Err(PersistError::ChunkCountMismatch {
                expected: total_chunks,
                actual: raw_rows.len(),
            });
        }

        // Parallel decompress & deserialize chunks
        let decoded: Result<Vec<(usize, ChunkBlobV3)>, PersistError> = raw_rows
            .into_par_iter()
            .map(|(idx, comp)| {
                let raw = zstd::stream::decode_all(&comp[..])?;
                let vblob: VersionedChunkBlob = postcard::from_bytes(&raw)?;
                let legacy_microbes = !matches!(vblob, VersionedChunkBlob::V3(_));
                let mut blob = vblob.migrate_to_current();
                if legacy_microbes {
                    // Preserve the migration cursor when metadata advances on the next save.
                    blob.l0.sleep.dirty_for_save = true;
                    let clock = meta.clock.config;
                    blob.l1.microbial_meso_step =
                        if blob.l0.last_realized_macro_step == meta.clock.macro_step.0 {
                            meta.clock.tick.0
                        } else {
                            blob.l0
                                .last_realized_macro_step
                                .saturating_mul(clock.ticks_per_macro_step())
                        } / u64::from(clock.meso_period_ticks.max(1));
                }
                Ok((idx, blob))
            })
            .collect();
        let mut decoded = decoded?;
        decoded.sort_unstable_by_key(|(idx, _)| *idx);

        let mut l0_chunks = Vec::with_capacity(total_chunks);
        let mut l1_chunks = Vec::with_capacity(total_chunks);
        for (_, blob) in decoded {
            l0_chunks.push(blob.l0);
            l1_chunks.push(blob.l1);
        }

        let mut world = WorldState {
            config: meta.config,
            seed: meta.seed,
            worldgen_version: meta.worldgen_version,
            clock: meta.clock,
            l0_chunks,
            l1_chunks,
            groundwater: Default::default(),
            regions: meta.regions,
            topology: meta.topology,
            surface_reference_height: meta.surface_reference_height,
            surface_salt_inventory: meta.surface_salt_inventory,
            initial_world_water_ul: meta.initial_world_water_ul,
        };
        if legacy_water {
            reconcile_legacy_water(&mut world)?;
        }
        if legacy_history {
            // Old versions discarded the prefix before saving. Rebase only across
            // that unrecoverable prefix, retaining all records still on disk.
            for (ci, chunk) in world.l0_chunks.iter_mut().enumerate() {
                let x = ci % chunks_x * CHUNK_SIZE;
                for region in &world.regions {
                    if x < region.x_end && x + CHUNK_SIZE > region.x_start {
                        let recovered = chunk
                            .last_realized_macro_step
                            .max(region.flux_history.discarded_through);
                        if recovered != chunk.last_realized_macro_step {
                            chunk.last_realized_macro_step = recovered;
                            chunk.sleep.dirty_for_save = true;
                        }
                    }
                }
            }
        }
        Ok(world)
    }

    fn encode_meta(world: &WorldState, zstd_level: i32) -> Result<Vec<u8>, PersistError> {
        let meta = VersionedWorldMeta::V4(WorldMetaV4 {
            schema_version: CURRENT_SCHEMA_VERSION,
            worldgen_version: world.worldgen_version,
            seed: world.seed,
            config: world.config.clone(),
            clock: world.clock,
            initial_world_water_ul: world.initial_world_water_ul,
            regions: world.regions.clone(),
            topology: world.topology.clone(),
            surface_reference_height: world.surface_reference_height.clone(),
            surface_salt_inventory: world.surface_salt_inventory.clone(),
        });
        let raw = postcard::to_allocvec(&meta)?;
        Ok(zstd::bulk::compress(&raw, zstd_level)?)
    }
}

/// Clear previously booked evaporation against the original basin first, then the rest of
/// the atmosphere. Legacy precipitation may already have moved part of that vapor to L0.
fn reconcile_legacy_water(world: &mut WorldState) -> Result<(), PersistError> {
    let mut debt = 0i64;
    for region in &mut world.regions {
        if region.pending_surface_water_ul < 0 {
            let overcredit = region
                .pending_surface_water_ul
                .checked_neg()
                .ok_or(PersistError::InvalidLegacyWater)?;
            let local = overcredit.min(region.water_vapor_ul);
            region.water_vapor_ul -= local;
            region.pending_surface_water_ul = 0;
            debt = debt
                .checked_add(overcredit - local)
                .ok_or(PersistError::InvalidLegacyWater)?;
        }
    }
    for region in &mut world.regions {
        let from_vapor = debt.min(region.water_vapor_ul);
        region.water_vapor_ul -= from_vapor;
        debt -= from_vapor;
    }
    for region in &mut world.regions {
        let from_pending = debt.min(region.pending_surface_water_ul);
        region.pending_surface_water_ul -= from_pending;
        debt -= from_pending;
    }
    for chunk in &mut world.l0_chunks {
        if debt == 0 {
            return Ok(());
        }
        for (i, volume) in chunk.fluid_volume.iter_mut().enumerate() {
            let removed = debt.min(i64::from(*volume));
            if removed == 0 {
                continue;
            }
            *volume -= removed as u16;
            debt -= removed;
            if *volume == 0 {
                chunk.fluid_kind[i] = FluidKind::None as u8;
            }
            chunk.dirty_rect.mark(i % CHUNK_SIZE, i / CHUNK_SIZE);
            chunk.sleep.fluid_sleeping = false;
            chunk.sleep.dirty_for_save = true;
            if debt == 0 {
                return Ok(());
            }
        }
    }
    if debt == 0 {
        Ok(())
    } else {
        Err(PersistError::InvalidLegacyWater)
    }
}
