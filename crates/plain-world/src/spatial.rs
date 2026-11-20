use plain_contracts::materials::{FluidKind, MaterialId, MATERIALS};
use plain_contracts::telemetry::{EraPhase, WeatherSpectacle};
use plain_contracts::time::MultiRateClock;
use plain_contracts::units::{
    DeciKelvin, FertilityUnits, MicroCoulombs, MicroLiters, PhScaled, SalinityPpm,
};
use plain_contracts::{
    CellCoord, ChunkCoord, FieldDownsample, MasterSeed, RegionId, WorldConfig, CHUNK_CELLS,
    CHUNK_SIZE,
};
use serde::{Deserialize, Serialize};

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

/// Bounding box of active cells inside one 64x64 chunk (Noita dirty-rect optimization).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtyRect {
    pub min_x: u8,
    pub min_y: u8,
    pub max_x: u8,
    pub max_y: u8,
    pub active: bool,
}

impl Default for DirtyRect {
    fn default() -> Self {
        Self::empty()
    }
}

impl DirtyRect {
    pub const fn empty() -> Self {
        Self {
            min_x: 63,
            min_y: 63,
            max_x: 0,
            max_y: 0,
            active: false,
        }
    }

    pub const fn full() -> Self {
        Self {
            min_x: 0,
            min_y: 0,
            max_x: (CHUNK_SIZE - 1) as u8,
            max_y: (CHUNK_SIZE - 1) as u8,
            active: true,
        }
    }

    #[inline]
    pub fn mark(&mut self, lx: usize, ly: usize) {
        let x = lx.min(CHUNK_SIZE - 1) as u8;
        let y = ly.min(CHUNK_SIZE - 1) as u8;
        if !self.active {
            self.min_x = x;
            self.max_x = x;
            self.min_y = y;
            self.max_y = y;
            self.active = true;
        } else {
            if x < self.min_x {
                self.min_x = x;
            }
            if x > self.max_x {
                self.max_x = x;
            }
            if y < self.min_y {
                self.min_y = y;
            }
            if y > self.max_y {
                self.max_y = y;
            }
        }
    }

    #[inline]
    pub fn expand_by(&mut self, pad: u8) {
        if !self.active {
            return;
        }
        self.min_x = self.min_x.saturating_sub(pad);
        self.min_y = self.min_y.saturating_sub(pad);
        self.max_x = self.max_x.saturating_add(pad).min((CHUNK_SIZE - 1) as u8);
        self.max_y = self.max_y.saturating_add(pad).min((CHUNK_SIZE - 1) as u8);
    }

    #[inline]
    pub fn clear(&mut self) {
        *self = Self::empty();
    }
}

/// Chunk sleep & persistence tracking flags (Factorio sleep scheduling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChunkSleepFlags {
    pub fluid_sleeping: bool,
    pub fields_sleeping: bool,
    pub dirty_for_save: bool,
    pub flux_mask_dirty: bool,
}

/// L0 fine 64x64 cell Structure-of-Arrays chunk (exact 8 bytes/cell = 32 KiB payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerrainChunkSoA {
    /// MaterialId raw byte (1 B/cell).
    pub solid_material: Vec<u8>,
    /// FluidKind raw byte (1 B/cell).
    pub fluid_kind: Vec<u8>,
    /// Structural integrity before physical/chemical degradation (2 B/cell).
    pub solid_integrity: Vec<u16>,
    /// Exact integer conserved fluid units, 0..=10_000 (2 B/cell).
    pub fluid_volume: Vec<u16>,
    /// Sunlight, ruin anchor, dormant seed bank, flood channel flags (1 B/cell).
    pub cell_flags: Vec<u8>,
    /// Micro surface state (salt sheen, moss tint, fracture hint) (1 B/cell).
    pub aux_mineral: Vec<u8>,

    pub dirty_rect: DirtyRect,
    pub sleep: ChunkSleepFlags,
    /// Last macro step at which this chunk was realized / caught up (E.4 lazy catch-up).
    pub last_realized_macro_step: u64,
}

impl TerrainChunkSoA {
    pub fn new_air() -> Self {
        Self {
            solid_material: vec![0; CHUNK_CELLS],
            fluid_kind: vec![0; CHUNK_CELLS],
            solid_integrity: vec![0; CHUNK_CELLS],
            fluid_volume: vec![0; CHUNK_CELLS],
            cell_flags: vec![0; CHUNK_CELLS],
            aux_mineral: vec![0; CHUNK_CELLS],
            dirty_rect: DirtyRect::empty(),
            sleep: ChunkSleepFlags {
                fluid_sleeping: true,
                fields_sleeping: true,
                dirty_for_save: false,
                flux_mask_dirty: true,
            },
            last_realized_macro_step: 0,
        }
    }

    #[inline]
    pub fn set_solid(&mut self, lx: usize, ly: usize, mat: MaterialId) {
        let idx = ly * CHUNK_SIZE + lx;
        self.solid_material[idx] = mat.0;
        self.solid_integrity[idx] = MATERIALS.get(mat).base_integrity;
        if mat.is_solid() {
            // Solid displaces fluid volume only if caller explicitly manages conservation;
            // worldgen uses this prior to hydrology fill.
        }
        self.dirty_rect.mark(lx, ly);
        self.sleep.fluid_sleeping = false;
        self.sleep.fields_sleeping = false;
        self.sleep.flux_mask_dirty = true;
        self.sleep.dirty_for_save = true;
    }

    #[inline]
    pub fn estimated_bytes(&self) -> usize {
        self.solid_material.capacity()
            + self.fluid_kind.capacity()
            + self.solid_integrity.capacity() * 2
            + self.fluid_volume.capacity() * 2
            + self.cell_flags.capacity()
            + self.aux_mineral.capacity()
            + std::mem::size_of::<Self>()
    }

    pub fn total_fluid_units(&self) -> i64 {
        self.fluid_volume.iter().map(|&v| v as i64).sum()
    }
}

/// L1 downsampled environment, microbial & local electric field chunk (F4 & Q1).
/// Resolution is configurable: 16x16 (By4) or 32x32 (By2) tiles per chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldChunkSoA {
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

impl FieldChunkSoA {
    pub fn new(downsample: FieldDownsample) -> Self {
        let axis = downsample.tiles_per_chunk_axis();
        let n = axis * axis;
        Self {
            axis_len: axis as u8,
            ph_scaled: vec![PhScaled::NEUTRAL.0; n],
            fertility: vec![18_000; n],
            salinity: vec![2_000; n],
            temperature_dk: vec![DeciKelvin::AMBIENT_20C.0; n],
            microbe_saprotroph: vec![30; n],
            microbe_conductive: vec![15; n],
            microbe_halophile: vec![15; n],
            microbe_ferro: vec![10; n],
            electric_potential: vec![0.0; n],
            conductivity: vec![60; n],
            flux_block_mask: vec![0; n],
        }
    }

    #[inline]
    pub fn idx(&self, fx: usize, fy: usize) -> usize {
        fy * (self.axis_len as usize) + fx
    }

    #[inline]
    pub fn estimated_bytes(&self) -> usize {
        self.ph_scaled.capacity()
            + self.fertility.capacity() * 2
            + self.salinity.capacity() * 2
            + self.temperature_dk.capacity() * 2
            + self.microbe_saprotroph.capacity()
            + self.microbe_conductive.capacity()
            + self.microbe_halophile.capacity()
            + self.microbe_ferro.capacity()
            + self.electric_potential.capacity() * 4
            + self.conductivity.capacity() * 2
            + self.flux_block_mask.capacity()
            + std::mem::size_of::<Self>()
    }
}

/// Geographical role of an L3 MacroRegion / Basin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasinRole {
    RidgePlateau,
    CanyonTravertineTerrace,
    ElectrolyteSaltBasin,
    FaultBasin,
}

/// Compact per-macro-step net flux record for dormant chunk lazy catch-up (E.4 & Q7).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MacroFluxRecord {
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

/// Bounded ring buffer of regional macro flux records (supports lazy catch-up across >= 2 full 1220-day cycles).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegionalMacroFluxHistory {
    pub records: Vec<MacroFluxRecord>,
    pub max_len: usize,
    /// Last step intentionally evicted by history retention.
    pub discarded_through: u64,
}

impl RegionalMacroFluxHistory {
    pub fn new(max_len: usize) -> Self {
        Self {
            records: Vec::with_capacity(max_len.min(4096)),
            max_len,
            discarded_through: 0,
        }
    }

    pub fn push(&mut self, rec: MacroFluxRecord) {
        if self.max_len > 0 && self.records.len() >= self.max_len {
            // Drop oldest quarter in batch to keep amortized O(1) while preserving contiguous slices.
            let drop_n = (self.max_len / 4).max(1);
            self.discarded_through = self.records[drop_n - 1].macro_step;
            self.records.drain(0..drop_n);
        }
        self.records.push(rec);
    }

    pub fn slice_since(&self, last_step: u64) -> &[MacroFluxRecord] {
        let idx = self.records.partition_point(|r| r.macro_step <= last_step);
        &self.records[idx..]
    }
}

/// L3 MacroRegion / Basin owning conserved atmospheric reservoirs and 4-era hysteresis state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroRegion {
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
    /// Water exchanged between atmosphere and dormant chunks pending lazy realization (exact conservation).
    pub pending_surface_water_ul: i64,
    /// Rainwater still in pending that has not accumulated to a full per-column increment.
    pub precipitation_remainder_ul: i64,
    /// Unissued surface evaporation demand. This is a request, not a water reservoir.
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

    pub flux_history: RegionalMacroFluxHistory,
}

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

/// Authoritative root state of `Rust World Core` (F2 Single Source of Truth).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldState {
    pub config: WorldConfig,
    pub seed: MasterSeed,
    pub worldgen_version: u32,
    pub clock: MultiRateClock,
    pub l0_chunks: Vec<TerrainChunkSoA>,
    pub l1_chunks: Vec<FieldChunkSoA>,
    pub regions: Vec<MacroRegion>,
    pub topology: WorldTopology,
    /// Baseline conserved total water recorded at genesis (surface fluid + vapor + pending).
    pub initial_world_water_ul: i64,
}

impl WorldState {
    pub fn new_empty(config: WorldConfig, seed: MasterSeed, worldgen_version: u32) -> Self {
        let total_chunks = config.dimensions.total_chunks();
        let ds = config.field_downsample;
        let l0_chunks = (0..total_chunks)
            .map(|_| TerrainChunkSoA::new_air())
            .collect();
        let l1_chunks = (0..total_chunks).map(|_| FieldChunkSoA::new(ds)).collect();
        let clock = MultiRateClock::new(config.clock);
        Self {
            config,
            seed,
            worldgen_version,
            clock,
            l0_chunks,
            l1_chunks,
            regions: Vec::new(),
            topology: WorldTopology::default(),
            initial_world_water_ul: 0,
        }
    }

    #[inline]
    pub fn width_cells(&self) -> usize {
        self.config.dimensions.width_cells()
    }

    #[inline]
    pub fn height_cells(&self) -> usize {
        self.config.dimensions.height_cells()
    }

    #[inline]
    pub fn chunk_index(&self, cc: ChunkCoord) -> Option<usize> {
        self.config.dimensions.chunk_index(cc)
    }

    #[inline]
    pub fn region_for_x(&self, x: usize) -> RegionId {
        for r in &self.regions {
            if x >= r.x_start && x < r.x_end {
                return r.id;
            }
        }
        RegionId(0)
    }

    #[inline]
    pub fn get_material(&self, cell: CellCoord) -> MaterialId {
        if !self.config.dimensions.contains_cell(cell) {
            // Mathematical zero-flux domain boundary outside finite array (NOT an indestructible gameplay block).
            return MaterialId::BASALT_BEDROCK;
        }
        let cc = cell.chunk_coord();
        let ci = self.config.dimensions.chunk_index(cc).unwrap_or(0);
        MaterialId(self.l0_chunks[ci].solid_material[cell.local_index()])
    }

    #[inline]
    pub fn get_fluid(&self, cell: CellCoord) -> (FluidKind, u16) {
        if !self.config.dimensions.contains_cell(cell) {
            return (FluidKind::None, 0);
        }
        let cc = cell.chunk_coord();
        let ci = self.config.dimensions.chunk_index(cc).unwrap_or(0);
        let li = cell.local_index();
        let ch = &self.l0_chunks[ci];
        (FluidKind::from_u8(ch.fluid_kind[li]), ch.fluid_volume[li])
    }

    #[inline]
    pub fn set_solid_unchecked_gen(&mut self, cell: CellCoord, mat: MaterialId) {
        if !self.config.dimensions.contains_cell(cell) {
            return;
        }
        let cc = cell.chunk_coord();
        if let Some(ci) = self.config.dimensions.chunk_index(cc) {
            let lx = cell.x.rem_euclid(CHUNK_SIZE as i32) as usize;
            let ly = cell.y.rem_euclid(CHUNK_SIZE as i32) as usize;
            self.l0_chunks[ci].set_solid(lx, ly, mat);
        }
    }

    #[inline]
    pub fn set_fluid_gen(&mut self, cell: CellCoord, kind: FluidKind, volume: u16) {
        if !self.config.dimensions.contains_cell(cell) {
            return;
        }
        let cc = cell.chunk_coord();
        if let Some(ci) = self.config.dimensions.chunk_index(cc) {
            let li = cell.local_index();
            let ch = &mut self.l0_chunks[ci];
            if ch.solid_material[li] != MaterialId::AIR.0 {
                return;
            }
            ch.fluid_kind[li] = if volume > 0 { kind as u8 } else { 0 };
            ch.fluid_volume[li] = volume.min(plain_contracts::CELL_MAX_FLUID_UNITS);
            let lx = cell.x.rem_euclid(CHUNK_SIZE as i32) as usize;
            let ly = cell.y.rem_euclid(CHUNK_SIZE as i32) as usize;
            ch.dirty_rect.mark(lx, ly);
            ch.sleep.fluid_sleeping = false;
        }
    }

    #[inline]
    pub fn set_cell_flag(&mut self, cell: CellCoord, flag: u8) {
        if !self.config.dimensions.contains_cell(cell) {
            return;
        }
        let cc = cell.chunk_coord();
        if let Some(ci) = self.config.dimensions.chunk_index(cc) {
            let li = cell.local_index();
            self.l0_chunks[ci].cell_flags[li] |= flag;
        }
    }

    /// Unified material-property physical stress application (F5: NO indestructible special cases).
    /// Applies mechanical pressure (kPa) and acid erosion flux uniformly against `MaterialDef`.
    /// Returns true if the cell transitioned material state.
    pub fn apply_physical_wear(
        &mut self,
        cell: CellCoord,
        applied_stress_kpa: u32,
        acid_flux: f32,
        electro_shock: f32,
    ) -> bool {
        if !self.config.dimensions.contains_cell(cell) {
            return false;
        }
        let cc = cell.chunk_coord();
        let Some(ci) = self.config.dimensions.chunk_index(cc) else {
            return false;
        };
        let li = cell.local_index();
        let mat_id = MaterialId(self.l0_chunks[ci].solid_material[li]);
        if mat_id.is_air() {
            return false;
        }
        let def = MATERIALS.get(mat_id);

        // Mechanical damage only occurs when applied stress overcomes a physical fraction of yield strength.
        let mech_dmg: u32 = if applied_stress_kpa > def.yield_strength_kpa / 4 {
            let excess = applied_stress_kpa.saturating_sub(def.yield_strength_kpa / 4);
            (excess / 25).max(1)
        } else {
            0
        };

        // Chemical dissolution scales strictly with `acid_susceptibility`.
        let acid_dmg: u32 = if acid_flux > 0.0 && def.acid_susceptibility > 0 {
            let scaled = acid_flux * (def.acid_susceptibility as f32) * 0.45;
            scaled.max(0.0) as u32
        } else {
            0
        };

        // Electro-thermal spalling scales inversely with `electro_thermal_res`.
        let electro_dmg: u32 = if electro_shock > 0.0 {
            let vulnerability = (1000u16.saturating_sub(def.electro_thermal_res)) as f32 / 1000.0;
            (electro_shock * vulnerability * 25.0).max(0.0) as u32
        } else {
            0
        };

        let total_dmg = mech_dmg
            .saturating_add(acid_dmg)
            .saturating_add(electro_dmg);
        if total_dmg == 0 {
            return false;
        }

        let ch = &mut self.l0_chunks[ci];
        let lx = cell.x.rem_euclid(CHUNK_SIZE as i32) as usize;
        let ly = cell.y.rem_euclid(CHUNK_SIZE as i32) as usize;
        ch.sleep.dirty_for_save = true;
        let cur = ch.solid_integrity[li] as u32;
        if total_dmg >= cur {
            let next_mat = def.erosion_product.unwrap_or(MaterialId::AIR);
            ch.solid_material[li] = next_mat.0;
            ch.solid_integrity[li] = MATERIALS.get(next_mat).base_integrity;
            ch.dirty_rect.mark(lx, ly);
            ch.sleep.fluid_sleeping = false;
            ch.sleep.fields_sleeping = false;
            ch.sleep.flux_mask_dirty = true;
            // Wake neighboring chunks if on border
            self.wake_neighbors_if_border(cc, lx, ly);
            true
        } else {
            ch.solid_integrity[li] = (cur - total_dmg) as u16;
            false
        }
    }

    pub fn wake_neighbors_if_border(&mut self, cc: ChunkCoord, lx: usize, ly: usize) {
        let offsets = [
            (lx == 0, -1, 0),
            (lx == CHUNK_SIZE - 1, 1, 0),
            (ly == 0, 0, -1),
            (ly == CHUNK_SIZE - 1, 0, 1),
        ];
        for (cond, dx, dy) in offsets {
            if cond {
                if let Some(nci) = self
                    .config
                    .dimensions
                    .chunk_index(ChunkCoord::new(cc.cx + dx, cc.cy + dy))
                {
                    let nch = &mut self.l0_chunks[nci];
                    nch.sleep.fluid_sleeping = false;
                    nch.sleep.fields_sleeping = false;
                    nch.sleep.flux_mask_dirty = true;
                    nch.sleep.dirty_for_save = true;
                    nch.dirty_rect = DirtyRect::full();
                }
            }
        }
    }

    /// Recompute L0-derived wall-aware `flux_block_mask` for a single chunk's L1 field tiles (F4).
    /// Prevents acid, heat, microbes, or electric current from teleporting across thin solid walls.
    pub fn recompute_chunk_flux_mask(&mut self, chunk_idx: usize) {
        let ds = self.config.field_downsample.factor();
        let axis = self.config.field_downsample.tiles_per_chunk_axis();
        let l0 = &self.l0_chunks[chunk_idx];
        let l1 = &mut self.l1_chunks[chunk_idx];

        let is_impermeable = |lx: usize, ly: usize| -> bool {
            let mat = MaterialId(l0.solid_material[ly * CHUNK_SIZE + lx]);
            mat.is_solid() && MATERIALS.get(mat).permeability < 200
        };

        for fy in 0..axis {
            for fx in 0..axis {
                let x0 = fx * ds;
                let y0 = fy * ds;
                let x1 = x0 + ds - 1;
                let y1 = y0 + ds - 1;

                let mut n_solid = 0usize;
                let mut s_solid = 0usize;
                let mut e_solid = 0usize;
                let mut w_solid = 0usize;

                for k in 0..ds {
                    if is_impermeable(x0 + k, y1) {
                        n_solid += 1;
                    }
                    if is_impermeable(x0 + k, y0) {
                        s_solid += 1;
                    }
                    if is_impermeable(x1, y0 + k) {
                        e_solid += 1;
                    }
                    if is_impermeable(x0, y0 + k) {
                        w_solid += 1;
                    }
                }

                // If an entire boundary edge (or >= 75% for By4) is impermeable rock/alloy, block L1 flux.
                let thresh = if ds <= 2 { ds } else { ds - 1 };
                let mut mask = 0u8;
                if n_solid >= thresh {
                    mask |= FLUX_BLOCK_N;
                }
                if s_solid >= thresh {
                    mask |= FLUX_BLOCK_S;
                }
                if e_solid >= thresh {
                    mask |= FLUX_BLOCK_E;
                }
                if w_solid >= thresh {
                    mask |= FLUX_BLOCK_W;
                }
                let fi = fy * axis + fx;
                l1.flux_block_mask[fi] = mask;
            }
        }
        self.l0_chunks[chunk_idx].sleep.flux_mask_dirty = false;
        self.l0_chunks[chunk_idx].sleep.dirty_for_save = true;
    }

    pub fn recompute_all_flux_masks(&mut self) {
        let n = self.l0_chunks.len();
        for ci in 0..n {
            self.recompute_chunk_flux_mask(ci);
        }
    }

    /// Exact integer sum of all surface/subsurface fluid + atmospheric vapor + pending catch-up water.
    pub fn total_world_water(&self) -> (MicroLiters, MicroLiters, MicroLiters) {
        let surface: i64 = self.l0_chunks.iter().map(|c| c.total_fluid_units()).sum();
        let pending: i64 = self
            .regions
            .iter()
            .map(|r| r.pending_surface_water_ul)
            .sum();
        let vapor: i64 = self.regions.iter().map(|r| r.water_vapor_ul).sum();
        let total_surface = surface + pending;
        (
            MicroLiters(total_surface),
            MicroLiters(vapor),
            MicroLiters(total_surface + vapor),
        )
    }

    pub fn estimated_memory_bytes(&self) -> usize {
        let l0: usize = self.l0_chunks.iter().map(|c| c.estimated_bytes()).sum();
        let l1: usize = self.l1_chunks.iter().map(|c| c.estimated_bytes()).sum();
        let topo = self.topology.surface_height.capacity() * 2
            + self.topology.aquifer_y.capacity() * 2
            + self.topology.ruin_anchors.capacity() * std::mem::size_of::<RuinGeologicalAnchor>();
        let reg: usize = self
            .regions
            .iter()
            .map(|r| {
                std::mem::size_of::<MacroRegion>()
                    + r.flux_history.records.capacity() * std::mem::size_of::<MacroFluxRecord>()
            })
            .sum();
        l0 + l1 + topo + reg + std::mem::size_of::<Self>()
    }

    #[inline]
    pub fn sample_l1_at_cell(
        &self,
        cell: CellCoord,
    ) -> Option<(
        PhScaled,
        FertilityUnits,
        SalinityPpm,
        DeciKelvin,
        [u8; 4],
        f32,
        u16,
        u8,
    )> {
        if !self.config.dimensions.contains_cell(cell) {
            return None;
        }
        let cc = cell.chunk_coord();
        let ci = self.config.dimensions.chunk_index(cc)?;
        let ds = self.config.field_downsample.factor();
        let fx = (cell.x.rem_euclid(CHUNK_SIZE as i32) as usize) / ds;
        let fy = (cell.y.rem_euclid(CHUNK_SIZE as i32) as usize) / ds;
        let f = &self.l1_chunks[ci];
        let fi = f.idx(fx, fy);
        Some((
            PhScaled(f.ph_scaled[fi]),
            FertilityUnits(f.fertility[fi]),
            SalinityPpm(f.salinity[fi]),
            DeciKelvin(f.temperature_dk[fi]),
            [
                f.microbe_saprotroph[fi],
                f.microbe_conductive[fi],
                f.microbe_halophile[fi],
                f.microbe_ferro[fi],
            ],
            f.electric_potential[fi],
            f.conductivity[fi],
            f.flux_block_mask[fi],
        ))
    }

    pub fn total_charge_sum(&self) -> MicroCoulombs {
        MicroCoulombs(self.regions.iter().map(|r| r.surface_charge_uc).sum())
    }
}
