#![deny(clippy::todo, clippy::unimplemented)]

//! Authoritative Rust World Core simulation engine (F1, F2, F4, F5, F6).
//!
//! Zero ECS framework dependencies, zero game-engine coupling.
//! Owns the 4-tier spatial grid (L0/L1/L2/L3), material-property terrain mechanics,
//! parallel conserved fluid solver, wall-aware L1 physicochemical/microbial/electric fields,
//! 1220-game-day non-uniform non-linear 4-era hysteresis climate engine, and lazy catch-up.

pub mod catchup;
pub mod climate;
pub mod electric;
pub mod fields;
pub mod fluid;
mod groundwater;
mod halo;
pub mod scheduler;
pub mod spatial;
#[cfg(feature = "dev-telemetry")]
pub mod telemetry;

pub use electric::ElectricSample;
pub use groundwater::GroundwaterConnectivity;
pub use scheduler::{ObserverCamera, ProfilerState, WorldSimulation};
pub use spatial::{
    ArchivedFluxRecord, BasinRole, ChunkSleepFlags, DarkRiverEdge, DirtyRect, FieldChunkSoA,
    L1CellSample, MacroFluxRecord, MacroRegion, RegionalMacroFluxHistory, RuinAnchorKind,
    RuinGeologicalAnchor, SinkholeChannel, SinkholeDescriptor, TerrainChunkSoA, WorldState,
    WorldTopology, FLUX_BLOCK_E, FLUX_BLOCK_N, FLUX_BLOCK_S, FLUX_BLOCK_W,
};
