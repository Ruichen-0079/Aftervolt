use serde::{Deserialize, Serialize};

use crate::coords::{FieldDownsample, WorldDimensions};
use crate::time::ClockConfig;

/// Current generator algorithm version (F8: frozen per save baseline).
pub const CURRENT_WORLDGEN_VERSION: u32 = 4;
/// Current persistence schema version (F8: explicit migration chain).
pub const CURRENT_SCHEMA_VERSION: u32 = 4;

/// Configuration for the non-uniform, non-linear 4-era macro cycle.
/// Confirmed baseline: default full cycle = 1220 game days, non-uniform era durations,
/// reservoir-threshold hysteresis dynamics (never linear interpolation).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FourEraCycleConfig {
    /// Target average full 4-era cycle duration in game days (default: 1220).
    pub target_cycle_days: u32,
    /// Relative target share of Era 4 (FlowerSeaBloom) — long high-productivity drawdown.
    pub bloom_weight: f32,
    /// Relative target share of Era 1 (SaltCrustDesert) — long arid electrostatic buildup.
    pub desert_weight: f32,
    /// Relative target share of Era 2 (SilentReverseRain) — short sharp upward-charge discharge.
    pub reverse_rain_weight: f32,
    /// Relative target share of Era 3 (RustRedRainFlood) — short violent acid flood & bloom trigger.
    pub red_rain_weight: f32,
    /// Hysteresis separation margin preventing chattering near phase thresholds (0.05..0.35).
    pub hysteresis_margin: f32,
    /// Non-linear steepness exponent for S-curve / pulse transitions (> 1.0).
    pub nonlinearity_exponent: f32,
    /// Fraction of atmospheric vapor & mineral dust transported downwind across basins per macro day.
    pub cross_basin_wind_transfer_per_day: f32,
    /// Peak acid rain pH dip strength during RustRedRainFlood (scaled pH drop intensity).
    pub red_rain_acid_intensity: f32,
}

impl Default for FourEraCycleConfig {
    fn default() -> Self {
        Self {
            target_cycle_days: 1220,
            bloom_weight: 0.46,
            desert_weight: 0.37,
            reverse_rain_weight: 0.07,
            red_rain_weight: 0.10,
            hysteresis_margin: 0.16,
            nonlinearity_exponent: 2.6,
            cross_basin_wind_transfer_per_day: 0.014,
            red_rain_acid_intensity: 1.0,
        }
    }
}

/// Top-level configurable world specification (F3: dimensions & parameters never frozen to one constant).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldConfig {
    pub dimensions: WorldDimensions,
    /// Physical meters per L0 cell (candidate: 0.5m or 0.25m for Q1).
    pub meters_per_cell: f32,
    /// L0 -> L1 downsampling ratio (default By4 = 4x4, testable By2 = 2x2 for Q1).
    pub field_downsample: FieldDownsample,
    /// Multi-rate clock settings (Q2 / Q7).
    pub clock: ClockConfig,
    /// Four-era 1220-day macro cycle parameters.
    pub four_era_cycle: FourEraCycleConfig,
    /// Number of L3 macro basins across the finite width.
    pub basin_count: u16,
    /// Target fraction of world volume/landmark footprint occupied by scattered ruin anchors (~10%).
    pub ruin_target_fraction: f32,
    /// Relaxation sweeps per active electric-field update pass (Q2 / Q3).
    pub electric_solver_iterations: u8,
    /// Active observer LOD-0 half-extent in chunks around each observer viewport.
    pub active_lod0_padding_chunks: u16,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self::candidate_6144x2048()
    }
}

impl WorldConfig {
    /// Primary candidate benchmark configuration (6144x2048 cells, 1220-day macro cycle).
    pub fn candidate_6144x2048() -> Self {
        Self {
            dimensions: WorldDimensions::candidate_6144x2048(),
            meters_per_cell: 0.5,
            field_downsample: FieldDownsample::By4,
            clock: ClockConfig::default(),
            four_era_cycle: FourEraCycleConfig::default(),
            basin_count: 4,
            ruin_target_fraction: 0.09,
            electric_solver_iterations: 6,
            active_lod0_padding_chunks: 4,
        }
    }

    pub fn preset_4096x1536() -> Self {
        Self {
            dimensions: WorldDimensions::preset_4096x1536(),
            basin_count: 3,
            ..Self::candidate_6144x2048()
        }
    }

    pub fn preset_8192x2048() -> Self {
        Self {
            dimensions: WorldDimensions::preset_8192x2048(),
            basin_count: 5,
            ..Self::candidate_6144x2048()
        }
    }

    /// Compact configuration for rapid 100-seed unit/CI sweeps while preserving full topology rules.
    pub fn preset_ci_1024x512() -> Self {
        Self {
            dimensions: WorldDimensions::preset_ci_1024x512(),
            basin_count: 3,
            active_lod0_padding_chunks: 2,
            ..Self::candidate_6144x2048()
        }
    }

    #[inline]
    pub fn world_width_meters(&self) -> f64 {
        (self.dimensions.width_cells() as f64) * (self.meters_per_cell as f64)
    }

    #[inline]
    pub fn world_height_meters(&self) -> f64 {
        (self.dimensions.height_cells() as f64) * (self.meters_per_cell as f64)
    }
}
