use plain_contracts::config::{CURRENT_SCHEMA_VERSION, CURRENT_WORLDGEN_VERSION};
use plain_contracts::{
    EraPhase, MasterSeed, MultiRateClock, RegionId, WeatherSpectacle, WorldConfig,
};
use plain_world::{
    BasinRole, FieldChunkSoA, MacroFluxRecord, MacroRegion, RegionalMacroFluxHistory,
    TerrainChunkSoA, WorldTopology,
};
use serde::{Deserialize, Serialize};

/// Versioned enum wrapper for explicit sequential schema migration (`V1 -> V2 -> ...`) (F8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VersionedWorldMeta {
    V1(WorldMetaV1),
    V2(WorldMetaV2),
}

impl VersionedWorldMeta {
    /// Convert legacy metadata; the store reconciles legacy water after loading L0 chunks.
    pub fn migrate_to_current(self) -> WorldMetaV2 {
        match self {
            Self::V1(v1) => WorldMetaV2 {
                schema_version: CURRENT_SCHEMA_VERSION,
                worldgen_version: v1.worldgen_version,
                seed: v1.seed,
                config: v1.config,
                clock: v1.clock,
                initial_world_water_ul: v1.initial_world_water_ul,
                regions: v1.regions.into_iter().map(MacroRegion::from).collect(),
                topology: v1.topology,
            },
            Self::V2(v2) => v2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldMetaV1 {
    pub schema_version: u32,
    pub worldgen_version: u32,
    pub seed: MasterSeed,
    pub config: WorldConfig,
    pub clock: MultiRateClock,
    pub initial_world_water_ul: i64,
    pub regions: Vec<MacroRegionV1>,
    pub topology: WorldTopology,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldMetaV2 {
    pub schema_version: u32,
    pub worldgen_version: u32,
    pub seed: MasterSeed,
    pub config: WorldConfig,
    pub clock: MultiRateClock,
    pub initial_world_water_ul: i64,
    pub regions: Vec<MacroRegion>,
    pub topology: WorldTopology,
}

impl WorldMetaV2 {
    pub const fn expected_versions() -> (u32, u32) {
        (CURRENT_SCHEMA_VERSION, CURRENT_WORLDGEN_VERSION)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroRegionV1 {
    pub id: RegionId,
    pub name: String,
    pub role: BasinRole,
    pub x_start: usize,
    pub x_end: usize,
    pub mean_surface_y: u16,
    pub water_vapor_ul: i64,
    pub pending_surface_water_ul: i64,
    pub mineral_dust_units: i64,
    pub surface_charge_uc: i64,
    pub moisture_index: f32,
    pub fertility_index: f32,
    pub salt_crust_ratio: f32,
    pub charge_index: f32,
    pub dust_saturation: f32,
    pub flower_sea_suitability: f32,
    pub era: EraPhase,
    pub weather: WeatherSpectacle,
    pub era_elapsed_macro_steps: u64,
    pub era_transitions_count: u32,
    pub flux_history: RegionalMacroFluxHistoryV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionalMacroFluxHistoryV1 {
    pub records: Vec<MacroFluxRecord>,
    pub max_len: usize,
}

impl From<MacroRegionV1> for MacroRegion {
    fn from(old: MacroRegionV1) -> Self {
        let mut records = old.flux_history.records;
        // V1 persisted contiguous histories, so a missing prefix was evicted by retention.
        let discarded_through = records
            .first()
            .map_or(0, |r| r.macro_step.saturating_sub(1));
        // Legacy evaporation records were already credited to vapor during the macro step.
        for record in &mut records {
            record.net_water_delta_per_col = record.net_water_delta_per_col.max(0);
        }
        Self {
            id: old.id,
            name: old.name,
            role: old.role,
            x_start: old.x_start,
            x_end: old.x_end,
            mean_surface_y: old.mean_surface_y,
            water_vapor_ul: old.water_vapor_ul,
            pending_surface_water_ul: old.pending_surface_water_ul,
            mineral_dust_units: old.mineral_dust_units,
            surface_charge_uc: old.surface_charge_uc,
            moisture_index: old.moisture_index,
            fertility_index: old.fertility_index,
            salt_crust_ratio: old.salt_crust_ratio,
            charge_index: old.charge_index,
            dust_saturation: old.dust_saturation,
            flower_sea_suitability: old.flower_sea_suitability,
            era: old.era,
            weather: old.weather,
            era_elapsed_macro_steps: old.era_elapsed_macro_steps,
            era_transitions_count: old.era_transitions_count,
            flux_history: RegionalMacroFluxHistory {
                records,
                max_len: old.flux_history.max_len,
                discarded_through,
            },
            precipitation_remainder_ul: 0,
            evaporation_remainder_ul: 0,
        }
    }
}

/// Versioned enum wrapper for per-chunk L0 + L1 binary blobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VersionedChunkBlob {
    V1(ChunkBlobV1),
}

impl VersionedChunkBlob {
    pub fn migrate_to_current(self) -> ChunkBlobV1 {
        match self {
            Self::V1(v1) => v1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkBlobV1 {
    pub l0: TerrainChunkSoA,
    pub l1: FieldChunkSoA,
}
