use plain_contracts::materials::{FluidKind, MaterialId, MATERIALS};
use plain_contracts::time::MultiRateClock;
use plain_contracts::units::{
    DeciKelvin, FertilityUnits, MicroCoulombs, MicroLiters, PhScaled, SalinityPpm,
};
use plain_contracts::{CellCoord, ChunkCoord, MasterSeed, RegionId, WorldConfig, CHUNK_SIZE};
use serde::{Deserialize, Serialize};

use crate::groundwater::GroundwaterConnectivity;

use super::{
    ArchivedFluxRecord, DirtyRect, FieldChunkSoA, MacroFluxRecord, MacroRegion,
    RuinGeologicalAnchor, TerrainChunkSoA, WorldTopology, MICROBE_SCALE,
};

/// L1 field values at the tile containing a world cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct L1CellSample {
    pub ph: PhScaled,
    pub fertility: FertilityUnits,
    pub salinity: SalinityPpm,
    pub temperature: DeciKelvin,
    pub microbes: [u8; 4],
    pub electric_potential: f32,
    pub conductivity: u16,
    pub flux_block_mask: u8,
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
    #[serde(skip)]
    #[doc(hidden)]
    pub groundwater: GroundwaterConnectivity,
    pub regions: Vec<MacroRegion>,
    pub topology: WorldTopology,
    /// Original column elevations used to keep a stable owner for surface replay.
    pub surface_reference_height: Vec<u16>,
    /// Undeposited salt supplied by arid evaporation, indexed by column.
    pub surface_salt_inventory: Vec<f64>,
    /// Baseline conserved total water recorded at genesis (surface fluid + vapor + pending).
    pub initial_world_water_ul: i64,
}

impl WorldState {
    pub fn new_empty(config: WorldConfig, seed: MasterSeed, worldgen_version: u32) -> Self {
        let total_chunks = config.dimensions.total_chunks();
        let ds = config.field_downsample;
        let width = config.dimensions.width_cells();
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
            groundwater: GroundwaterConnectivity::default(),
            regions: Vec::new(),
            topology: WorldTopology::default(),
            surface_reference_height: Vec::new(),
            surface_salt_inventory: vec![0.0; width],
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
            let before = MaterialId(self.l0_chunks[ci].solid_material[ly * CHUNK_SIZE + lx]);
            self.l0_chunks[ci].set_solid(lx, ly, mat);
            self.groundwater_material_changed(cell, before, mat);
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
            ch.tile_conditions.clear();
            ch.sleep.dirty_for_save = true;
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
            ch.tile_conditions.clear();
            ch.solid_integrity[li] = MATERIALS.get(next_mat).base_integrity;
            ch.dirty_rect.mark(lx, ly);
            ch.sleep.fluid_sleeping = false;
            ch.sleep.fields_sleeping = false;
            ch.sleep.flux_mask_dirty = true;
            self.groundwater_material_changed(cell, mat_id, next_mat);
            // Wake neighboring chunks if on border
            self.wake_neighbors_if_border(cc, lx, ly);
            if self.topology.surface_height.get(cell.x as usize) == Some(&(cell.y as u16))
                && next_mat.is_air()
            {
                self.refresh_surface_height(cell.x as usize, cell.y as usize);
            }
            true
        } else {
            ch.solid_integrity[li] = (cur - total_dmg) as u16;
            false
        }
    }

    fn groundwater_material_changed(
        &mut self,
        cell: CellCoord,
        before: MaterialId,
        after: MaterialId,
    ) {
        self.groundwater.terrain_changed(
            &self.l0_chunks,
            self.config.dimensions.chunks_x as usize,
            (cell.x as usize, cell.y as usize),
            before,
            after,
        );
    }

    /// Recompute a column top after removing its exposed solid.
    pub(crate) fn refresh_surface_height(&mut self, x: usize, ceiling: usize) {
        self.topology.surface_height[x] = (0..ceiling)
            .rev()
            .find(|&y| {
                self.get_material(CellCoord::new(x as i32, y as i32))
                    .is_solid()
            })
            .unwrap_or(0) as u16;
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
        self.l0_chunks[chunk_idx].recompute_flux_mask(&mut self.l1_chunks[chunk_idx], ds);
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
        let topo = (self.topology.surface_height.capacity()
            + self.surface_reference_height.capacity())
            * 2
            + self.surface_salt_inventory.capacity() * 8
            + self.topology.aquifer_y.capacity() * 2
            + self.topology.ruin_anchors.capacity() * std::mem::size_of::<RuinGeologicalAnchor>();
        let reg: usize = self
            .regions
            .iter()
            .map(|r| {
                std::mem::size_of::<MacroRegion>()
                    + r.flux_history.records.capacity() * std::mem::size_of::<MacroFluxRecord>()
                    + r.flux_history.archived_buckets.capacity()
                        * std::mem::size_of::<ArchivedFluxRecord>()
            })
            .sum();
        l0 + l1 + self.groundwater.estimated_bytes() + topo + reg + std::mem::size_of::<Self>()
    }

    #[inline]
    pub fn sample_l1_at_cell(&self, cell: CellCoord) -> Option<L1CellSample> {
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
        Some(L1CellSample {
            ph: PhScaled(((f.ph_milli[fi] + 50) / 100) as u8),
            fertility: FertilityUnits(f.fertility[fi]),
            salinity: SalinityPpm(f.salinity[fi]),
            temperature: DeciKelvin(f.temperature_dk[fi]),
            microbes: [
                (f.microbe_saprotroph[fi] / MICROBE_SCALE) as u8,
                (f.microbe_conductive[fi] / MICROBE_SCALE) as u8,
                (f.microbe_halophile[fi] / MICROBE_SCALE) as u8,
                (f.microbe_ferro[fi] / MICROBE_SCALE) as u8,
            ],
            electric_potential: f.electric_potential[fi],
            conductivity: f.conductivity[fi],
            flux_block_mask: f.flux_block_mask[fi],
        })
    }

    pub fn total_charge_sum(&self) -> MicroCoulombs {
        MicroCoulombs(self.regions.iter().map(|r| r.surface_charge_uc).sum())
    }
}
