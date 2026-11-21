use plain_contracts::materials::{MaterialId, MATERIALS};
use plain_contracts::units::{DeciKelvin, PhScaled};
use plain_contracts::{FieldDownsample, CHUNK_CELLS, CHUNK_SIZE};
use serde::{Deserialize, Serialize};

use crate::fields::TileConditions;

use super::{FLUX_BLOCK_E, FLUX_BLOCK_N, FLUX_BLOCK_S, FLUX_BLOCK_W, MICROBE_SCALE};

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
    #[serde(skip)]
    pub(crate) tile_conditions: Vec<TileConditions>,
    #[serde(skip)]
    pub(crate) fluid_active_last_tick: bool,
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
            tile_conditions: Vec::new(),
            fluid_active_last_tick: false,
        }
    }

    #[inline]
    pub fn set_solid(&mut self, lx: usize, ly: usize, mat: MaterialId) {
        let idx = ly * CHUNK_SIZE + lx;
        self.solid_material[idx] = mat.0;
        self.tile_conditions.clear();
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

    pub(crate) fn recompute_flux_mask(&mut self, l1: &mut FieldChunkSoA, ds: usize) {
        let axis = CHUNK_SIZE / ds;

        let is_impermeable = |lx: usize, ly: usize| -> bool {
            let mat = MaterialId(self.solid_material[ly * CHUNK_SIZE + lx]);
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
        self.sleep.flux_mask_dirty = false;
        self.sleep.dirty_for_save = true;
    }

    #[inline]
    pub fn estimated_bytes(&self) -> usize {
        self.tile_conditions.capacity() * std::mem::size_of::<TileConditions>()
            + self.solid_material.capacity()
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
    pub ph_milli: Vec<u16>,
    pub fertility: Vec<u16>,
    pub salinity: Vec<u16>,
    pub temperature_dk: Vec<u16>,
    pub microbe_saprotroph: Vec<u16>,
    pub microbe_conductive: Vec<u16>,
    pub microbe_halophile: Vec<u16>,
    pub microbe_ferro: Vec<u16>,
    /// Number of elapsed meso updates covered by the microbial fields.
    pub microbial_meso_step: u64,
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
            ph_milli: vec![u16::from(PhScaled::NEUTRAL.0) * 100; n],
            fertility: vec![18_000; n],
            salinity: vec![2_000; n],
            temperature_dk: vec![DeciKelvin::AMBIENT_20C.0; n],
            microbe_saprotroph: vec![30 * MICROBE_SCALE; n],
            microbe_conductive: vec![15 * MICROBE_SCALE; n],
            microbe_halophile: vec![15 * MICROBE_SCALE; n],
            microbe_ferro: vec![10 * MICROBE_SCALE; n],
            microbial_meso_step: 0,
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
        self.ph_milli.capacity() * 2
            + self.fertility.capacity() * 2
            + self.salinity.capacity() * 2
            + self.temperature_dk.capacity() * 2
            + self.microbe_saprotroph.capacity() * 2
            + self.microbe_conductive.capacity() * 2
            + self.microbe_halophile.capacity() * 2
            + self.microbe_ferro.capacity() * 2
            + self.electric_potential.capacity() * 4
            + self.conductivity.capacity() * 2
            + self.flux_block_mask.capacity()
            + std::mem::size_of::<Self>()
    }
}
