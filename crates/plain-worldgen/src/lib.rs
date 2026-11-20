#![deny(clippy::todo, clippy::unimplemented)]

//! Deterministic finite 2D world generation pipeline and automated world validation gates.
//!
//! Pipeline (DAG):
//! 1. Hierarchical BLAKE3/ChaCha20 domain sub-seeds
//! 2. L3 macroscopic basin layout & elevation targets
//! 3. L0 surface sculpting, hydraulic erosion, travertine terraces, stone pillars & arches
//! 4. Underground karst caverns, vertical sinkholes, dark rivers & mineralized fossil cavities
//! 5. ~10% scattered ruin geological anchors (cavities & deep waste-heat boundaries)
//! 6. Hydrology fill & static equilibrium relaxation
//! 7. L1 wall-aware flux masks, physicochemical/microbial fields & asynchronous L3 reservoirs
//! 8. Automated world legality validation

pub mod fields_init;
pub mod hydrology_init;
pub mod macro_layout;
pub mod noise;
pub mod ruin_anchors;
pub mod terrain_sculpt;
pub mod underground;
pub mod validator;

use plain_contracts::config::CURRENT_WORLDGEN_VERSION;
use plain_contracts::{MasterSeed, WorldConfig};
use plain_world::WorldState;
use thiserror::Error;

pub use validator::{ValidationCheck, WorldValidationReport};

#[derive(Debug, Error)]
pub enum WorldGenError {
    #[error("invalid world dimensions: chunks_x={0}, chunks_y={1} (minimum 8x6; height and area must fit generation indices)")]
    InvalidDimensions(u16, u16),
    #[error("invalid world cell scale: meters_per_cell={0} (must be finite and positive)")]
    InvalidCellScale(f32),
    #[error("world validation failed after {attempts} attempts; last failures: {summary}")]
    ValidationFailed { attempts: u32, summary: String },
}

/// Generate a complete finite 2D world deterministically from `(config, seed)` and validate it.
pub fn generate_world(
    config: WorldConfig,
    seed: MasterSeed,
) -> Result<(WorldState, WorldValidationReport), WorldGenError> {
    let dims = config.dimensions;
    let cell_count = dims.width_cells() as u64 * dims.height_cells() as u64;
    // Each cell can originate up to five transfers in a fluid tick. The tally is a u32.
    let max_fluid_cells = u64::from(u32::MAX) / 5;
    if dims.chunks_x < 8
        || dims.chunks_y < 6
        || dims.height_cells() > usize::from(u16::MAX) + 1
        || cell_count > max_fluid_cells
        || cell_count > isize::MAX as u64 / 8
    {
        return Err(WorldGenError::InvalidDimensions(
            dims.chunks_x,
            dims.chunks_y,
        ));
    }

    if !config.meters_per_cell.is_finite() || config.meters_per_cell <= 0.0 {
        return Err(WorldGenError::InvalidCellScale(config.meters_per_cell));
    }

    let mut world = WorldState::new_empty(config, seed, CURRENT_WORLDGEN_VERSION);

    // Step 2: Macroscopic L3 basin layout & base elevation curves
    macro_layout::build_macro_layout(&mut world);

    // Step 3: Surface sculpting, hydraulic erosion, travertine pools, arches & pillars
    terrain_sculpt::sculpt_surface_and_strata(&mut world);

    // Step 4: Karst caves, deep dark-river aquifer, vertical sinkholes & fossil cavities
    underground::carve_underground_and_sinkholes(&mut world);

    // Step 5: Scattered ~10% ruin geological anchors (no Stage 4 mechanics)
    ruin_anchors::embed_ruin_geological_anchors(&mut world);

    // Step 6: Hydrology fill & static equilibrium settling
    hydrology_init::initialize_hydrology(&mut world);

    // Step 7: L1 wall-aware flux masks, physicochemical/microbial fields & L3 reservoirs
    fields_init::initialize_fields_and_climate(&mut world);

    // Clear genesis dirty_for_save flags because the genesis state is persisted as `world_baseline`.
    for ch in &mut world.l0_chunks {
        ch.sleep.dirty_for_save = false;
        ch.last_realized_macro_step = 0;
    }

    // Step 8: Run automated legality validation gates
    let report = validator::validate_world(&world);
    Ok((world, report))
}

/// Generate a world with deterministic sub-seed retry if a rare seed violates any hard legality gate.
pub fn generate_valid_world(
    config: WorldConfig,
    master_seed: MasterSeed,
    max_attempts: u32,
) -> Result<(WorldState, WorldValidationReport, u32), WorldGenError> {
    let mut last_summary = String::new();
    for attempt in 0..max_attempts.max(1) {
        let attempt_seed = if attempt == 0 {
            master_seed
        } else {
            let mut hasher = blake3::Hasher::new_keyed(&master_seed.0);
            hasher.update(b"worldgen-retry-attempt:");
            hasher.update(&attempt.to_le_bytes());
            MasterSeed(*hasher.finalize().as_bytes())
        };
        let (world, report) = generate_world(config.clone(), attempt_seed)?;
        if report.passed {
            return Ok((world, report, attempt));
        }
        last_summary = report.failure_summary();
    }
    Err(WorldGenError::ValidationFailed {
        attempts: max_attempts.max(1),
        summary: last_summary,
    })
}
