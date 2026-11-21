use serde::{Deserialize, Serialize};

use crate::coords::{CellCoord, PersistentUid, RegionId, WorldPosMeters};
use crate::materials::{FluidKind, MaterialId};
use crate::time::{GameDay, MacroStep, Tick};
use crate::units::{DeciKelvin, FertilityUnits, MicroCoulombs, MicroLiters, PhScaled, SalinityPpm};

/// Four-era macro geological-climatic cycle phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum EraPhase {
    /// Era 1: Arid salt-crust hardening & electrostatic dust buildup.
    SaltCrustDesert = 0,
    /// Era 2: Short upward electrostatic reverse rain & dust uplift.
    SilentReverseRain = 1,
    /// Era 3: Short violent acidic iron-mineral rain, crust dissolution & flooding.
    RustRedRainFlood = 2,
    /// Era 4: Long electrolyte flower-sea prosperity & near-ground conductive mist.
    FlowerSeaBloom = 3,
}

impl EraPhase {
    pub const fn label_cn(self) -> &'static str {
        match self {
            Self::SaltCrustDesert => "第一纪元：盐壳静电荒漠",
            Self::SilentReverseRain => "第二纪元：静默逆雨浮尘",
            Self::RustRedRainFlood => "第三纪元：铁锈红雨洪泛",
            Self::FlowerSeaBloom => "第四纪元：花海鼎盛球雷",
        }
    }

    pub const fn debug_rgb(self) -> [u8; 3] {
        match self {
            Self::SaltCrustDesert => [226, 214, 178],
            Self::SilentReverseRain => [138, 176, 245],
            Self::RustRedRainFlood => [206, 84, 64],
            Self::FlowerSeaBloom => [76, 198, 168],
        }
    }
}

/// Active regional meteorological spectacle coupled to L3 reservoirs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum WeatherSpectacle {
    Calm = 0,
    SilentReverseRain = 1,
    WanderingBallLightning = 2,
    AuroraMagStorm = 3,
    RustRedRain = 4,
}

impl WeatherSpectacle {
    pub const fn label_cn(self) -> &'static str {
        match self {
            Self::Calm => "平稳静电风",
            Self::SilentReverseRain => "静默逆雨 / 浮尘潮",
            Self::WanderingBallLightning => "游走球雷 / 贴地电雾",
            Self::AuroraMagStorm => "极光磁暴 / 幻影夜",
            Self::RustRedRain => "铁锈红雨 / 酸蚀暴",
        }
    }
}

/// Diagnostic overlay modes supported by the developer observer (F9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum OverlayKind {
    CompositeTerrainFluid = 0,
    MaterialYieldStrength = 1,
    PhField = 2,
    FertilityField = 3,
    SalinityField = 4,
    TemperatureField = 5,
    MicrobeSaprotroph = 6,
    MicrobeConductive = 7,
    MicrobeHalophile = 8,
    MicrobeFerro = 9,
    ElectricPotential = 10,
    ChunkActivityDebug = 11,
    BiomeSuitability = 12,
    ConductivityField = 13,
}

impl OverlayKind {
    pub const ALL: [Self; 14] = [
        Self::CompositeTerrainFluid,
        Self::MaterialYieldStrength,
        Self::PhField,
        Self::FertilityField,
        Self::SalinityField,
        Self::TemperatureField,
        Self::MicrobeSaprotroph,
        Self::MicrobeConductive,
        Self::MicrobeHalophile,
        Self::MicrobeFerro,
        Self::ElectricPotential,
        Self::ConductivityField,
        Self::ChunkActivityDebug,
        Self::BiomeSuitability,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::CompositeTerrainFluid => "综合地形与水体 (L0)",
            Self::MaterialYieldStrength => "材料屈服强度 (F5物理属性)",
            Self::PhField => "酸碱度 pH 场 (L1)",
            Self::FertilityField => "土壤肥力/电解质 (L1)",
            Self::SalinityField => "含盐度场 (L1)",
            Self::TemperatureField => "温度与地热场 (L1)",
            Self::MicrobeSaprotroph => "微生物：腐生分解菌",
            Self::MicrobeConductive => "微生物：导电丝菌毯",
            Self::MicrobeHalophile => "微生物：固盐产气藻菌",
            Self::MicrobeFerro => "微生物：蚀铁矿化菌",
            Self::ElectricPotential => "局部物理电势场 (L1)",
            Self::ConductivityField => "电导率场 (L1)",
            Self::ChunkActivityDebug => "Chunk 活跃/休眠与脏区",
            Self::BiomeSuitability => "自然群系涌现宜居度",
        }
    }
}

/// FFI-friendly viewport rectangle in L0 cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct ViewportRect {
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

/// Discrete command packets sent from observer/frontend to WorldCore (F2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ObserverCommand {
    SetDevCamera { id: u8, viewport: ViewportRect },
    RemoveDevCamera { id: u8 },
    StepFineTicks { count: u32 },
    StepMacroSteps { count: u64 },
}

/// Flat RGBA raster buffer snapshot suitable for zero-copy display and future C ABI export.
#[derive(Debug, Clone)]
pub struct ViewportFieldSnapshot {
    pub width_px: u32,
    pub height_px: u32,
    pub overlay: OverlayKind,
    pub viewport: ViewportRect,
    /// Packed RGBA8 pixel data (length = width_px * height_px * 4).
    pub rgba: Vec<u8>,
}

/// Full-world overview thumbnail snapshot with anchor overlays.
#[derive(Debug, Clone)]
pub struct WorldOverviewSnapshot {
    pub width_px: u32,
    pub height_px: u32,
    pub rgba: Vec<u8>,
}

/// Conservation & macroscopic ledger exposed to developer telemetry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConservationLedger {
    pub tick: Tick,
    pub macro_step: MacroStep,
    pub game_day: GameDay,
    pub total_surface_fluid: MicroLiters,
    pub total_atmospheric_vapor: MicroLiters,
    pub total_world_water: MicroLiters,
    pub initial_world_water: MicroLiters,
    pub total_fertility_sum: u64,
    pub total_salinity_sum: u64,
    pub total_atmospheric_dust: i64,
    pub total_surface_charge: MicroCoulombs,
    pub total_salt_crust_cells: u64,
    pub active_chunks: u32,
    pub sleeping_chunks: u32,
    pub dirty_save_chunks: u32,
    pub regions: Vec<RegionTelemetry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionTelemetry {
    pub id: RegionId,
    pub name: String,
    pub x_start: usize,
    pub x_end: usize,
    pub era: EraPhase,
    pub weather: WeatherSpectacle,
    pub era_age_days: f32,
    pub water_vapor: MicroLiters,
    pub mineral_dust: i64,
    pub surface_charge: MicroCoulombs,
    pub moisture_index: f32,
    pub salt_crust_ratio: f32,
    pub flower_sea_suitability: f32,
    pub avg_ph: f32,
    pub avg_fertility: f32,
}

/// Detailed god-view diagnostic report for a single probed world cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellProbeReport {
    pub world_pos: WorldPosMeters,
    pub cell: CellCoord,
    pub region_id: RegionId,
    pub material: MaterialId,
    pub material_name: String,
    pub yield_strength_kpa: u32,
    pub acid_susceptibility: u16,
    pub integrity: u16,
    pub fluid_kind: FluidKind,
    pub fluid_volume: u16,
    pub ph: PhScaled,
    pub fertility: FertilityUnits,
    pub salinity: SalinityPpm,
    pub temperature: DeciKelvin,
    pub microbe_saprotroph: u8,
    pub microbe_conductive: u8,
    pub microbe_halophile: u8,
    pub microbe_ferro: u8,
    pub electric_potential: f32,
    pub electric_grad_x: f32,
    pub electric_grad_y: f32,
    pub flux_block_mask: u8,
    pub chunk_sleeping: bool,
    pub chunk_last_macro_step: u64,
    pub era: EraPhase,
    pub weather: WeatherSpectacle,
}

/// Per-subsystem wall-clock microsecond timing for a single step.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct SubsystemTimingsUs {
    pub fluid_us: u64,
    pub fields_us: u64,
    pub electric_us: u64,
    pub macro_climate_us: u64,
    pub catchup_us: u64,
    pub snapshot_us: u64,
    pub total_us: u64,
}

/// Rolling profiler telemetry for developer observer performance analysis (F9).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfilerSnapshot {
    pub latest: SubsystemTimingsUs,
    pub p50_total_us: u64,
    pub p95_total_us: u64,
    pub p99_total_us: u64,
    pub estimated_memory_bytes: usize,
    pub active_lod0_chunks: u32,
    pub total_chunks: u32,
    pub fine_ticks_executed: u64,
    pub macro_steps_executed: u64,
}

/// Compile-time isolated God-View Telemetry Port (F7 & F9).
/// Strictly gated under `dev-telemetry`; future player frontends and AI cognition MUST NOT
/// depend on this trait.
#[cfg(feature = "dev-telemetry")]
pub trait DevTelemetryPort {
    fn render_world_overview(
        &self,
        target_w: u32,
        target_h: u32,
        overlay: OverlayKind,
    ) -> WorldOverviewSnapshot;

    fn render_viewport(
        &self,
        viewport: ViewportRect,
        target_w: u32,
        target_h: u32,
        overlay: OverlayKind,
    ) -> ViewportFieldSnapshot;

    fn probe_cell(&self, cell: CellCoord) -> Option<CellProbeReport>;

    fn conservation_ledger(&self) -> ConservationLedger;

    fn profiler_snapshot(&self) -> ProfilerSnapshot;
}

/// Minimal compile-time boundary marker for future Stage 2/5 diegetic player perception (F7).
/// Player frontends will ONLY consume subject-bound observations through this port,
/// never WorldTruth via `DevTelemetryPort`.
pub trait PlayerPerceptionPort {
    fn bound_subject(&self) -> Option<PersistentUid>;
}
