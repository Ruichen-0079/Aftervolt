use plain_contracts::{CellCoord, RegionId};
use serde::{Deserialize, Serialize};

/// Geological ruin anchor categories (Stage 1 reserves physical cavities & waste heat; zero Stage 4 mechanics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuinAnchorKind {
    DeepRuinCore,
    CliffAirlockOutcrop,
    BuriedTankPit,
    PlateauTowerFoundation,
    SubmergedSluiceAnchor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuinGeologicalAnchor {
    pub id: u32,
    pub kind: RuinAnchorKind,
    pub region_id: RegionId,
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
    pub cavity_center: CellCoord,
    /// Constant deep waste-heat emission in 0.1 K above ambient (0 for cold surface outcrops).
    pub waste_heat_bonus_dk: u16,
}

/// Descriptor for a vertical karst sinkhole shaft piercing from surface to deep black water.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkholeDescriptor {
    pub region_id: RegionId,
    pub x_center: i32,
    pub top_y: i32,
    pub bottom_y: i32,
    pub mean_radius: u16,
}

/// Horizontal dark-river connectivity edge in the deep aquifer graph.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DarkRiverEdge {
    pub from_region: RegionId,
    pub to_region: RegionId,
    pub y_level: i32,
    pub min_clearance_cells: u16,
}

/// High-level macroscopic topology descriptors layered over L0/L1 grids (D.2 hybrid representation).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorldTopology {
    /// Column surface elevation profile (length = width_cells).
    pub surface_height: Vec<u16>,
    /// Deep black-water aquifer ceiling profile (length = width_cells).
    pub aquifer_y: Vec<u16>,
    /// Travertine terrace column ranges (x_start, x_end).
    pub travertine_zones: Vec<(usize, usize)>,
    /// Seasonal flood wadi column centers.
    pub flood_wadis: Vec<i32>,
    /// Vertical karst sinkholes connecting surface to deep black water.
    pub sinkholes: Vec<SinkholeDescriptor>,
    /// Deep underground dark-river connectivity edges.
    pub dark_river_edges: Vec<DarkRiverEdge>,
    /// Fossilized giant mineralized creature cavities (center, radius).
    pub fossil_cavities: Vec<(CellCoord, u16)>,
    /// Reserved ~10% scattered ruin geological anchors.
    pub ruin_anchors: Vec<RuinGeologicalAnchor>,
    /// Summary count of solid ruin cells embedded at genesis.
    pub initial_ruin_solid_cells: u64,
    /// Summary count of total solid cells at genesis.
    pub initial_total_solid_cells: u64,
    /// Fraction of surface columns naturally suitable for Electrolyte Flower Sea (ruin-independent).
    pub natural_flower_sea_column_ratio: f32,
}
