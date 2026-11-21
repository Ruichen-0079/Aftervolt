mod chunk;
mod region;
mod topology;
mod world;

pub use chunk::{ChunkSleepFlags, DirtyRect, FieldChunkSoA, TerrainChunkSoA};
pub use region::{
    ArchivedFluxRecord, ArchivedMoisture, BasinRole, MacroFluxRecord, MacroRegion,
    RegionalMacroFluxHistory,
};
pub use topology::SinkholeDescriptor as SinkholeChannel;
pub use topology::{
    DarkRiverEdge, RuinAnchorKind, RuinGeologicalAnchor, SinkholeDescriptor, WorldTopology,
};
pub use world::{L1CellSample, WorldState};

/// Wall-aware L1 flux blocking mask bits (prevents cross-wall leakage of heat/pH/microbes/E-field).
pub const FLUX_BLOCK_N: u8 = 0b0001;
pub const FLUX_BLOCK_S: u8 = 0b0010;
pub const FLUX_BLOCK_E: u8 = 0b0100;
pub const FLUX_BLOCK_W: u8 = 0b1000;

/// L0 cell auxiliary flags.
pub const CELL_FLAG_SUNLIT: u8 = 0b0001;
pub const CELL_FLAG_RUIN_ANCHOR: u8 = 0b0010;
pub const CELL_FLAG_DORMANT_BANK: u8 = 0b0100;
pub const CELL_FLAG_FLOOD_CHANNEL: u8 = 0b1000;

/// Number of stored units per whole microbial density unit.
pub const MICROBE_SCALE: u16 = 100;
