use plain_contracts::config::{CURRENT_SCHEMA_VERSION, CURRENT_WORLDGEN_VERSION};
use plain_contracts::{
    EraPhase, MasterSeed, MultiRateClock, RegionId, WeatherSpectacle, WorldConfig,
};
use plain_world::{
    ArchivedFluxRecord, BasinRole, FieldChunkSoA, MacroFluxRecord, MacroRegion,
    RegionalMacroFluxHistory, TerrainChunkSoA, WorldTopology,
};
use serde::{Deserialize, Serialize};

/// Versioned enum wrapper for explicit sequential schema migration (`V1 -> V2 -> ...`) (F8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VersionedWorldMeta {
    V1(WorldMetaV1),
    V2(WorldMetaV2),
    V3(WorldMetaV3),
    V4(WorldMetaV4),
}

impl VersionedWorldMeta {
    /// Convert legacy metadata; the store reconciles legacy water after loading L0 chunks.
    pub fn migrate_to_current(self) -> WorldMetaV4 {
        match self {
            Self::V1(v1) => WorldMetaV4 {
                schema_version: CURRENT_SCHEMA_VERSION,
                worldgen_version: v1.worldgen_version,
                seed: v1.seed,
                surface_salt_inventory: vec![0.0; v1.config.dimensions.width_cells()],
                config: v1.config,
                clock: v1.clock,
                initial_world_water_ul: v1.initial_world_water_ul,
                regions: v1.regions.into_iter().map(MacroRegion::from).collect(),
                surface_reference_height: v1.topology.surface_height.clone(),
                topology: v1.topology,
            },
            Self::V2(v2) => WorldMetaV4 {
                schema_version: CURRENT_SCHEMA_VERSION,
                worldgen_version: v2.worldgen_version,
                seed: v2.seed,
                surface_salt_inventory: vec![0.0; v2.config.dimensions.width_cells()],
                config: v2.config,
                clock: v2.clock,
                initial_world_water_ul: v2.initial_world_water_ul,
                regions: v2.regions.into_iter().map(MacroRegion::from).collect(),
                surface_reference_height: v2.topology.surface_height.clone(),
                topology: v2.topology,
            },
            Self::V3(old) => WorldMetaV4 {
                schema_version: CURRENT_SCHEMA_VERSION,
                worldgen_version: old.worldgen_version,
                seed: old.seed,
                config: old.config,
                clock: old.clock,
                initial_world_water_ul: old.initial_world_water_ul,
                regions: old
                    .regions
                    .into_iter()
                    .map(|region| {
                        region.map_flux_history(|history| RegionalMacroFluxHistory {
                            records: history
                                .records
                                .into_iter()
                                .map(|record| MacroFluxRecord {
                                    macro_step: record.macro_step,
                                    era: record.era,
                                    weather: record.weather,
                                    acid_erosion_impulse: record.acid_erosion_impulse,
                                    salt_deposition_impulse: record.salt_deposition_impulse,
                                    flood_peak: record.flood_peak,
                                    ph_delta: record.ph_delta,
                                    fertility_delta: record.fertility_delta,
                                    salinity_delta: record.salinity_delta,
                                    ambient_charge_v: record.ambient_charge_v,
                                    net_water_delta_per_col: record.net_water_delta_per_col,
                                    settled_water_ul: record.settled_water_ul,
                                    moisture: None,
                                })
                                .collect(),
                            archived_buckets: history
                                .archived_buckets
                                .into_iter()
                                .map(|archive| ArchivedFluxRecord {
                                    start_step: archive.start_step,
                                    end_step: archive.end_step,
                                    era: archive.era,
                                    acid: archive.acid,
                                    salt: archive.salt,
                                    flood_peak: archive.flood_peak,
                                    flood_exposure: archive.flood_exposure,
                                    ph: archive.ph,
                                    fertility: archive.fertility,
                                    salinity: archive.salinity,
                                    charge: archive.charge,
                                    water_per_col: archive.water_per_col,
                                    settled_water_ul: archive.settled_water_ul,
                                    moisture: Default::default(),
                                })
                                .collect(),
                            max_len: history.max_len,
                            discarded_through: history.discarded_through,
                        })
                    })
                    .collect(),
                surface_reference_height: old.surface_reference_height,
                surface_salt_inventory: old.surface_salt_inventory,
                topology: old.topology,
            },
            Self::V4(meta) => meta,
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
    pub regions: Vec<MacroRegionV2>,
    pub topology: WorldTopology,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldMetaV4<H = RegionalMacroFluxHistory> {
    pub schema_version: u32,
    pub worldgen_version: u32,
    pub seed: MasterSeed,
    pub config: WorldConfig,
    pub clock: MultiRateClock,
    pub initial_world_water_ul: i64,
    pub regions: Vec<MacroRegion<H>>,
    pub surface_reference_height: Vec<u16>,
    pub surface_salt_inventory: Vec<f64>,
    pub topology: WorldTopology,
}

// Postcard encodes unit fields as zero bytes. The shared shape therefore retains
// the V3 layout, which ended before the moisture data on both record types.
pub type WorldMetaV3 =
    WorldMetaV4<RegionalMacroFluxHistory<MacroFluxRecord<()>, ArchivedFluxRecord<()>>>;

impl<H> WorldMetaV4<H> {
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
    pub records: Vec<MacroFluxRecordV2>,
    pub max_len: usize,
}

impl From<MacroRegionV1> for MacroRegion {
    fn from(old: MacroRegionV1) -> Self {
        let mut records: Vec<MacroFluxRecord> = old
            .flux_history
            .records
            .into_iter()
            .map(MacroFluxRecord::from)
            .collect();
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
                archived_buckets: Vec::new(),
            },
            precipitation_remainder_ul: 0,
            evaporation_remainder_ul: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroRegionV2 {
    pub id: RegionId,
    pub name: String,
    pub role: BasinRole,
    /// Inclusive-exclusive horizontal span in L0 cells.
    pub x_start: usize,
    pub x_end: usize,
    /// Base reference surface elevation in L0 cells.
    pub mean_surface_y: u16,

    // Conserved L3 reservoirs (exact i64 accounting)
    pub water_vapor_ul: i64,
    /// Condensed water awaiting placement or re-evaporation (exact conservation).
    pub pending_surface_water_ul: i64,
    /// Rainwater eligible for the next basin-wide settlement.
    pub precipitation_remainder_ul: i64,
    /// Accumulated evaporation demand for the current day, in microliters.
    pub evaporation_remainder_ul: i64,
    pub mineral_dust_units: i64,
    pub surface_charge_uc: i64,

    // Normalized macroscopic state variables (0.0..=1.0) driving non-linear 4-era hysteresis
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

    pub flux_history: RegionalMacroFluxHistoryV2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionalMacroFluxHistoryV2 {
    pub records: Vec<MacroFluxRecordV2>,
    pub max_len: usize,
    pub discarded_through: u64,
}

impl From<MacroRegionV2> for MacroRegion {
    fn from(old: MacroRegionV2) -> Self {
        Self {
            id: old.id,
            name: old.name,
            role: old.role,
            x_start: old.x_start,
            x_end: old.x_end,
            mean_surface_y: old.mean_surface_y,
            water_vapor_ul: old.water_vapor_ul,
            pending_surface_water_ul: old.pending_surface_water_ul,
            precipitation_remainder_ul: old.precipitation_remainder_ul,
            evaporation_remainder_ul: old.evaporation_remainder_ul,
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
                records: old
                    .flux_history
                    .records
                    .into_iter()
                    .map(MacroFluxRecord::from)
                    .collect(),
                max_len: old.flux_history.max_len,
                discarded_through: old.flux_history.discarded_through,
                archived_buckets: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MacroFluxRecordV2 {
    pub macro_step: u64,
    pub era: u8,
    pub weather: u8,
    /// Acid rain dissolution impulse (>0 dissolves SaltCrust & weakens soil dams).
    pub acid_erosion_impulse: f32,
    /// Arid evaporation salt crust deposition impulse (>0 deposits surface salt crust).
    pub salt_deposition_impulse: f32,
    /// Flood peak hydraulic shear (0.0..=1.0).
    pub flood_peak: f32,
    /// Net pH shift impulse applied to exposed/shallow L1 tiles.
    pub ph_delta: f32,
    /// Net fertility impulse (+ during red-rain/bloom recovery, - during drawdown).
    pub fertility_delta: f32,
    /// Net salinity impulse (+ during arid evaporation, - during freshwater flush).
    pub salinity_delta: f32,
    /// Ambient surface electrostatic forcing potential (Volts-equivalent).
    pub ambient_charge_v: f32,
    /// Net rainfall (+) or evaporation (-) micro-liters per surface cell for conservation sync.
    pub net_water_delta_per_col: i32,
}

impl From<MacroFluxRecordV2> for MacroFluxRecord {
    fn from(old: MacroFluxRecordV2) -> Self {
        Self {
            macro_step: old.macro_step,
            era: old.era,
            weather: old.weather,
            acid_erosion_impulse: old.acid_erosion_impulse,
            salt_deposition_impulse: old.salt_deposition_impulse,
            flood_peak: old.flood_peak,
            ph_delta: old.ph_delta,
            fertility_delta: old.fertility_delta,
            salinity_delta: old.salinity_delta,
            ambient_charge_v: old.ambient_charge_v,
            net_water_delta_per_col: old.net_water_delta_per_col,
            settled_water_ul: 0,
            moisture: None,
        }
    }
}

/// Versioned enum wrapper for per-chunk L0 + L1 binary blobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VersionedChunkBlob {
    V1(ChunkBlobV1),
    V2(ChunkBlobV2),
    V3(ChunkBlobV3),
}

impl VersionedChunkBlob {
    pub fn migrate_to_current(self) -> ChunkBlobV3 {
        match self {
            Self::V1(v1) => ChunkBlobV3 {
                l0: v1.l0,
                l1: v1.l1.into(),
            },
            Self::V2(v2) => ChunkBlobV3 {
                l0: v2.l0,
                l1: v2.l1.into(),
            },
            Self::V3(v3) => v3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkBlobV1 {
    pub l0: TerrainChunkSoA,
    pub l1: FieldChunkV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkBlobV2 {
    pub l0: TerrainChunkSoA,
    pub l1: FieldChunkV2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkBlobV3 {
    pub l0: TerrainChunkSoA,
    pub l1: FieldChunkSoA,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldChunkV1 {
    pub axis_len: u8,
    pub ph_scaled: Vec<u8>,
    pub fertility: Vec<u16>,
    pub salinity: Vec<u16>,
    pub temperature_dk: Vec<u16>,
    pub microbe_saprotroph: Vec<u8>,
    pub microbe_conductive: Vec<u8>,
    pub microbe_halophile: Vec<u8>,
    pub microbe_ferro: Vec<u8>,
    pub electric_potential: Vec<f32>,
    pub conductivity: Vec<u16>,
    /// L0-derived directional wall blocking bitmask (N/S/E/W) preventing thin-wall flux leakage.
    pub flux_block_mask: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldChunkV2 {
    pub axis_len: u8,
    pub ph_milli: Vec<u16>,
    pub fertility: Vec<u16>,
    pub salinity: Vec<u16>,
    pub temperature_dk: Vec<u16>,
    pub microbe_saprotroph: Vec<u8>,
    pub microbe_conductive: Vec<u8>,
    pub microbe_halophile: Vec<u8>,
    pub microbe_ferro: Vec<u8>,
    pub electric_potential: Vec<f32>,
    pub conductivity: Vec<u16>,
    /// L0-derived directional wall blocking bitmask (N/S/E/W) preventing thin-wall flux leakage.
    pub flux_block_mask: Vec<u8>,
}

impl From<FieldChunkV1> for FieldChunkSoA {
    fn from(old: FieldChunkV1) -> Self {
        FieldChunkV2::from(old).into()
    }
}

impl From<FieldChunkV1> for FieldChunkV2 {
    fn from(old: FieldChunkV1) -> Self {
        Self {
            ph_milli: old
                .ph_scaled
                .into_iter()
                .map(|ph| u16::from(ph) * 100)
                .collect(),
            axis_len: old.axis_len,
            fertility: old.fertility,
            salinity: old.salinity,
            temperature_dk: old.temperature_dk,
            microbe_saprotroph: old.microbe_saprotroph,
            microbe_conductive: old.microbe_conductive,
            microbe_halophile: old.microbe_halophile,
            microbe_ferro: old.microbe_ferro,
            electric_potential: old.electric_potential,
            conductivity: old.conductivity,
            flux_block_mask: old.flux_block_mask,
        }
    }
}

impl From<FieldChunkV2> for FieldChunkSoA {
    fn from(old: FieldChunkV2) -> Self {
        let upgrade = |values: Vec<u8>| {
            values
                .into_iter()
                .map(|value| u16::from(value) * plain_world::spatial::MICROBE_SCALE)
                .collect()
        };
        Self {
            ph_milli: old.ph_milli,
            microbial_meso_step: 0,
            axis_len: old.axis_len,
            fertility: old.fertility,
            salinity: old.salinity,
            temperature_dk: old.temperature_dk,
            microbe_saprotroph: upgrade(old.microbe_saprotroph),
            microbe_conductive: upgrade(old.microbe_conductive),
            microbe_halophile: upgrade(old.microbe_halophile),
            microbe_ferro: upgrade(old.microbe_ferro),
            electric_potential: old.electric_potential,
            conductivity: old.conductivity,
            flux_block_mask: old.flux_block_mask,
        }
    }
}
