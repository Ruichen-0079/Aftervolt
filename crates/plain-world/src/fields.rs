use plain_contracts::materials::{FluidKind, MaterialId, MATERIALS};
use plain_contracts::telemetry::EraPhase;
use plain_contracts::{FieldDownsample, CHUNK_SIZE};
use rayon::prelude::*;

use crate::spatial::{
    FieldChunkSoA, TerrainChunkSoA, WorldState, FLUX_BLOCK_E, FLUX_BLOCK_N, FLUX_BLOCK_S,
    FLUX_BLOCK_W, MICROBE_SCALE,
};

// Lookups preserve MaterialDef as the conductivity authority.
const MATERIAL_CONDUCTIVITY: [u16; 256] = {
    let mut table = [0; 256];
    let mut id = 0;
    while id < table.len() {
        table[id] = MATERIALS.get(MaterialId(id as u8)).conductivity;
        id += 1;
    }
    table
};

struct FieldUpdates {
    ph: Vec<u16>,
    temp: Vec<u16>,
    fert: Vec<u16>,
    sal: Vec<u16>,
}

impl FieldUpdates {
    fn new(count: usize) -> Self {
        Self {
            ph: vec![0; count],
            temp: vec![0; count],
            fert: vec![0; count],
            sal: vec![0; count],
        }
    }

    fn publish(&mut self, chunk: &mut FieldChunkSoA) {
        std::mem::swap(&mut chunk.ph_milli, &mut self.ph);
        std::mem::swap(&mut chunk.temperature_dk, &mut self.temp);
        std::mem::swap(&mut chunk.fertility, &mut self.fert);
        std::mem::swap(&mut chunk.salinity, &mut self.sal);
    }
}

const fn is_metal(material: MaterialId) -> bool {
    material.is_ruin_material()
        || material.0 == MaterialId::CONDUCTIVE_ORE.0
        || material.0 == MaterialId::MINERALIZED_BONE.0
}

/// Step wall-aware L1 physicochemical (pH, fertility, salinity, temperature) & 4 microbial fields
/// across all chunks marked in `active_mask`.
///
/// Strictly enforces `flux_block_mask` so heat, pH, salinity, and microbes NEVER leak across
/// thin impermeable rock or ruin walls (F4).
pub fn step_l1_fields(world: &mut WorldState, active_mask: &[bool]) {
    step_fields(world, active_mask, None);
}

pub(crate) fn step_scheduled_fields(world: &mut WorldState, active_mask: &[bool]) {
    let meso_step = world.clock.tick.0 / u64::from(world.clock.config.meso_period_ticks.max(1));
    step_fields(world, active_mask, Some(meso_step));
}

fn step_fields(world: &mut WorldState, active_mask: &[bool], meso_step: Option<u64>) {
    let resolution = world.config.field_downsample;
    let chunks_x = world.config.dimensions.chunks_x as usize;

    // Ensure any chunk whose L0 solids changed has an up-to-date L1 flux_block_mask.
    for ci in 0..world.l0_chunks.len() {
        if active_mask[ci] && world.l0_chunks[ci].sleep.flux_mask_dirty {
            world.recompute_chunk_flux_mask(ci);
        }
    }

    let ds = resolution.factor();
    let axis = resolution.tiles_per_chunk_axis();
    let region_params: Vec<_> = (0..chunks_x * axis)
        .map(|fx| {
            let x = fx * ds + ds / 2;
            let id = world.region_for_x(x).0 as usize;
            world
                .regions
                .get(id)
                .map_or((EraPhase::FlowerSeaBloom, 0.0), |r| {
                    (r.era, r.moisture_index)
                })
        })
        .collect();

    let world_h = world.height_cells();
    let halos = crate::halo::snapshot(world, active_mask, |l1, i| {
        [
            l1.ph_milli[i],
            l1.temperature_dk[i],
            l1.fertility[i],
            l1.salinity[i],
        ]
    });

    // Parallel intra-chunk wall-aware field evolution & microbial dynamics.
    world
        .l1_chunks
        .par_iter_mut()
        .zip(world.l0_chunks.par_iter_mut())
        .enumerate()
        .for_each_init(
            || FieldUpdates::new(resolution.tiles_per_chunk_axis().pow(2)),
            |next, (ci, (l1, l0))| {
                if !active_mask[ci] {
                    return;
                }
                let cx = ci % chunks_x;
                let cy = ci / chunks_x;
                let region_params = &region_params[cx * axis..(cx + 1) * axis];
                let elapsed =
                    meso_step.map_or(1, |step| step.saturating_sub(l1.microbial_meso_step));
                let advance = MicrobialAdvance::new(elapsed);
                match resolution {
                    FieldDownsample::By2 => evolve_single_l1_chunk::<2>(
                        l1,
                        l0,
                        cy,
                        world_h,
                        region_params,
                        &advance,
                        &halos[ci],
                        next,
                    ),
                    FieldDownsample::By4 => evolve_single_l1_chunk::<4>(
                        l1,
                        l0,
                        cy,
                        world_h,
                        region_params,
                        &advance,
                        &halos[ci],
                        next,
                    ),
                }
                next.publish(l1);
                if let Some(step) = meso_step {
                    l1.microbial_meso_step = step;
                }
                l0.sleep.dirty_for_save = true;
            },
        );
}

#[allow(clippy::too_many_arguments)]
fn evolve_single_l1_chunk<const DS: usize>(
    l1: &mut FieldChunkSoA,
    l0: &TerrainChunkSoA,
    cy: usize,
    world_h: usize,
    region_params: &[(EraPhase, f32)],
    advance: &MicrobialAdvance,
    halo: &[Option<[u16; 4]>],
    next: &mut FieldUpdates,
) {
    let axis = CHUNK_SIZE / DS;
    let cell_count_per_tile = (DS * DS) as u32;

    for fy in 0..axis {
        let world_y = cy * CHUNK_SIZE + fy * DS + DS / 2;
        // Ambient altitude lapse rate + geothermal gradient near deep bedrock
        let norm_y = (world_y as f32) / (world_h.max(1) as f32);
        let ambient_dk = if norm_y < 0.22 {
            // Deep geothermal warmth (~24 C to 34 C)
            2970.0 + (0.22 - norm_y) * 420.0
        } else {
            // Surface to highland lapse (~22 C down to 6 C)
            2960.0 - (norm_y - 0.22) * 200.0
        };

        for (fx, &(era, region_moisture)) in region_params.iter().enumerate() {
            let fi = fy * axis + fx;
            let mask = l1.flux_block_mask[fi];

            // Aggregate local L0 physical conditions for this FieldTile.
            let local = TileConditions::sample::<DS>(l0, fx, fy);
            let TileConditions {
                wetness,
                solid_cnt,
                salt_crust_cnt,
                heat_core_cnt,
                ..
            } = local;

            // --- Wall-aware 4-neighbor diffusion (zero leakage across blocked walls!) ---
            let mut totals = [
                u32::from(l1.ph_milli[fi]) * 4,
                u32::from(l1.temperature_dk[fi]) * 4,
                u32::from(l1.fertility[fi]) * 4,
                u32::from(l1.salinity[fi]) * 4,
            ];
            let mut weight = 4;
            let mut add = |values: [u16; 4]| {
                for (total, value) in totals.iter_mut().zip(values) {
                    *total += u32::from(value);
                }
                weight += 1;
            };

            for (edge, delta, outgoing, incoming, slot) in [
                (
                    fy + 1 == axis,
                    axis as isize,
                    FLUX_BLOCK_N,
                    FLUX_BLOCK_S,
                    fx,
                ),
                (
                    fy == 0,
                    -(axis as isize),
                    FLUX_BLOCK_S,
                    FLUX_BLOCK_N,
                    axis + fx,
                ),
                (fx + 1 == axis, 1, FLUX_BLOCK_E, FLUX_BLOCK_W, 2 * axis + fy),
                (fx == 0, -1, FLUX_BLOCK_W, FLUX_BLOCK_E, 3 * axis + fy),
            ] {
                if edge {
                    if let Some(tile) = halo[slot] {
                        add(tile);
                    }
                } else if mask & outgoing == 0 {
                    let neighbor = fi.wrapping_add_signed(delta);
                    if l1.flux_block_mask[neighbor] & incoming == 0 {
                        add([
                            l1.ph_milli[neighbor],
                            l1.temperature_dk[neighbor],
                            l1.fertility[neighbor],
                            l1.salinity[neighbor],
                        ]);
                    }
                }
            }
            let inv_weight = 1.0 / weight as f32;
            let [mut ph_f, mut temp_f, mut fert_f, mut sal_f] =
                totals.map(|total| total as f32 * inv_weight);
            ph_f *= 0.01;

            let feedback = advance.apply(l1, fi, &local, region_moisture, ph_f, sal_f);
            if feedback.sap > 0.0 {
                temp_f += feedback.sap * 0.015;
                fert_f = (fert_f + feedback.sap * 0.45).min(62_000.0);
                ph_f = (ph_f - 0.02).max(52.0);
            }
            if feedback.halophile > 0.0 {
                fert_f = (fert_f + feedback.halophile * 0.25).min(60_000.0);
            }
            if feedback.ferro > 0.0 {
                fert_f = (fert_f + feedback.ferro * 0.18).min(60_000.0);
            }

            // Salt crust pushes local salinity & alkalinity up
            if salt_crust_cnt > 0 {
                sal_f = (sal_f + salt_crust_cnt as f32 * 45.0).min(64_000.0);
                ph_f = (ph_f + 0.08).min(108.0);
            }

            // Era-level soft local coupling on exposed shallow tiles
            if norm_y > 0.35 && u32::from(solid_cnt) < cell_count_per_tile {
                match era {
                    EraPhase::RustRedRainFlood => {
                        ph_f = (ph_f - 0.18).max(44.0);
                        fert_f = (fert_f + 35.0).min(62_000.0);
                        sal_f = (sal_f * 0.992).max(400.0);
                    }
                    EraPhase::SaltCrustDesert => {
                        sal_f = (sal_f + 22.0).min(64_000.0);
                        ph_f = (ph_f + 0.06).min(105.0);
                    }
                    EraPhase::FlowerSeaBloom => {
                        // Balanced rich basin
                        ph_f += (68.0 - ph_f) * 0.02;
                    }
                    EraPhase::SilentReverseRain => {}
                }
            }

            // Thermal relaxation toward ambient + deep ruin waste-heat emission
            let target_temp = ambient_dk + (heat_core_cnt as f32) * 180.0;
            let thermal_alpha = if wetness > 0.3 { 0.03 } else { 0.08 };
            temp_f += (target_temp - temp_f) * thermal_alpha;

            next.ph[fi] = (ph_f.clamp(25.0, 125.0) * 100.0 + 0.5) as u16;
            next.temp[fi] = (temp_f.clamp(2200.0, 4200.0) + 0.5) as u16;
            next.fert[fi] = (fert_f.clamp(0.0, 65_535.0) + 0.5) as u16;
            next.sal[fi] = (sal_f.clamp(0.0, 65_535.0) + 0.5) as u16;

            local.refresh_conductivity(l1, fi, f32::from(next.sal[fi]));
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TileConditions {
    solid_cnt: u8,
    metal_cnt: u8,
    salt_crust_cnt: u8,
    heat_core_cnt: u8,
    base_conductivity: u16,
    wetness: f32,
    has_black_water: bool,
}

impl TileConditions {
    #[inline]
    fn sample<const DS: usize>(l0: &TerrainChunkSoA, fx: usize, fy: usize) -> Self {
        let cell_count_per_tile = (DS * DS) as u32;
        let mut fluid_sum = 0u32;
        let mut solid_cnt = 0u8;
        let mut metal_cnt = 0u8;
        let mut salt_crust_cnt = 0u8;
        let mut heat_core_cnt = 0u8;
        let mut cond_accum = 0u32;
        let mut has_black_water = false;

        let x0 = fx * DS;
        let y0 = fy * DS;
        for dy in 0..DS {
            let row = (y0 + dy) * CHUNK_SIZE + x0;
            let tiles = row..row + DS;
            for ((&material, &volume), &kind) in l0.solid_material[tiles.clone()]
                .iter()
                .zip(&l0.fluid_volume[tiles.clone()])
                .zip(&l0.fluid_kind[tiles])
            {
                let mat = MaterialId(material);
                if mat.is_solid() {
                    solid_cnt += 1;
                    cond_accum += u32::from(MATERIAL_CONDUCTIVITY[usize::from(material)]);
                    salt_crust_cnt += u8::from(mat == MaterialId::SALT_CRUST);
                    heat_core_cnt += u8::from(mat == MaterialId::RUIN_HEAT_CORE);
                    metal_cnt += u8::from(mat != MaterialId::RUIN_HEAT_CORE && is_metal(mat));
                } else if volume > 0 {
                    let kind = FluidKind::from_u8(kind);
                    fluid_sum += u32::from(volume);
                    has_black_water |= kind == FluidKind::BlackWater && volume > 1000;
                    cond_accum +=
                        (u32::from(kind.base_conductivity()) * u32::from(volume)) / 10_000;
                } else {
                    cond_accum += u32::from(MATERIAL_CONDUCTIVITY[0]);
                }
            }
        }

        let wetness = ((fluid_sum as f32) / (cell_count_per_tile as f32 * 10_000.0)).min(1.0);
        Self {
            solid_cnt,
            metal_cnt,
            salt_crust_cnt,
            heat_core_cnt,
            base_conductivity: (cond_accum / cell_count_per_tile) as u16,
            wetness,
            has_black_water,
        }
    }

    fn refresh_conductivity(&self, l1: &mut FieldChunkSoA, fi: usize, salinity: f32) {
        let sal_boost = ((salinity / 65_535.0) * 320.0 * (0.2 + self.wetness)) as u32;
        let mat_density = f32::from(l1.microbe_conductive[fi]) / f32::from(MICROBE_SCALE);
        let mic_boost = ((mat_density / 255.0) * 260.0) as u32;
        l1.conductivity[fi] =
            (u32::from(self.base_conductivity) + sal_boost + mic_boost).min(1500) as u16;
    }
}

const BASIN_MOISTURE_WEIGHT: f32 = 0.35;
const METAL_MOISTURE: f32 = 0.12;
const SAP_MOISTURE: f32 = 0.20;
const NONMETAL_MOISTURE: f32 = 0.25;

pub(crate) fn moisture_band(moisture: Option<f32>) -> Option<u8> {
    moisture.map(|m| {
        let effective = m * BASIN_MOISTURE_WEIGHT;
        u8::from(effective > METAL_MOISTURE)
            + u8::from(effective > SAP_MOISTURE)
            + u8::from(effective > NONMETAL_MOISTURE)
    })
}

const DECAY: [(u16, u16); 4] = [(40, 4), (15, 3), (30, 4), (20, 2)];
const MAX_DENSITY: usize = 255 * MICROBE_SCALE as usize;
// Even the slowest guild takes fewer than 512 updates to decay from
// 255.00 to 3.00. Binary lifting handles shorter intervals exactly.
const DECAY_POWERS: usize = 9;
const DECAY_SATURATION_STEPS: u16 = 1 << DECAY_POWERS;
type DecayTable = [Vec<Vec<u16>>; 4];

fn decay_table() -> &'static DecayTable {
    static TABLE: std::sync::OnceLock<DecayTable> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|guild| {
            let mut powers = Vec::with_capacity(DECAY_POWERS);
            powers.push(
                (0..=MAX_DENSITY)
                    .map(|v| decay_once(guild, v as u16))
                    .collect::<Vec<_>>(),
            );
            for power in 1..DECAY_POWERS {
                let half = &powers[power - 1];
                powers.push(half.iter().map(|&v| half[usize::from(v)]).collect());
            }
            powers
        })
    })
}

fn decay_once(guild: usize, density: u16) -> u16 {
    let (loss, floor) = DECAY[guild];
    ((u32::from(density) * u32::from(1000 - loss) + 500) / 1000)
        .max(u32::from(floor * MICROBE_SCALE)) as u16
}

pub(crate) struct MicrobialAdvance {
    steps: u16,
    table: Option<&'static DecayTable>,
}

#[derive(Default)]
struct MicrobialFeedback {
    sap: f32,
    halophile: f32,
    ferro: f32,
}

impl MicrobialAdvance {
    pub(crate) fn new(steps: u64) -> Self {
        let steps = steps.min(MAX_DENSITY as u64) as u16;
        Self {
            steps,
            table: (2..DECAY_SATURATION_STEPS)
                .contains(&steps)
                .then(decay_table),
        }
    }

    fn grow(&self, density: u16, rate: f32, cap: u16) -> u16 {
        let per_step = (rate * f32::from(MICROBE_SCALE) + 0.5) as u32;
        (u32::from(density) + per_step * u32::from(self.steps)).min(u32::from(cap * MICROBE_SCALE))
            as u16
    }

    fn decay(&self, guild: usize, mut density: u16) -> u16 {
        if self.steps >= DECAY_SATURATION_STEPS {
            return DECAY[guild].1 * MICROBE_SCALE;
        }
        let Some(table) = self.table else {
            return decay_once(guild, density);
        };
        let mut steps = self.steps;
        while steps != 0 {
            let power = steps.trailing_zeros() as usize;
            density = table[guild][power][usize::from(density)];
            steps &= steps - 1;
        }
        density
    }

    fn apply(
        &self,
        l1: &mut FieldChunkSoA,
        fi: usize,
        local: &TileConditions,
        region_moisture: f32,
        ph: f32,
        salinity: f32,
    ) -> MicrobialFeedback {
        let mut feedback = MicrobialFeedback::default();
        if self.steps == 0 {
            return feedback;
        }
        let moisture = local.wetness.max(region_moisture * BASIN_MOISTURE_WEIGHT);
        let ph_ok_sap = (1.0 - ((ph - 68.0).abs() / 32.0)).clamp(0.0, 1.0);
        let sap_active = moisture > SAP_MOISTURE && ph_ok_sap > 0.25 && salinity < 38_000.0;
        let sap = &mut l1.microbe_saprotroph[fi];
        *sap = if sap_active {
            let next = self.grow(*sap, 1.6 * moisture * ph_ok_sap, 240);
            feedback.sap = f32::from(next) / f32::from(MICROBE_SCALE);
            next
        } else {
            self.decay(0, *sap)
        };

        let ph = if sap_active {
            (ph - 0.02).max(52.0)
        } else {
            ph
        };
        let conductive = &mut l1.microbe_conductive[fi];
        *conductive = if local.metal_cnt > 0 && ph < 72.0 && moisture > METAL_MOISTURE {
            self.grow(*conductive, 1.4, 245)
        } else if local.solid_cnt > 0 && ph < 66.0 && moisture > NONMETAL_MOISTURE {
            self.grow(*conductive, 0.5, 180)
        } else {
            self.decay(1, *conductive)
        };

        let halophile = &mut l1.microbe_halophile[fi];
        if (local.wetness > 0.15 && salinity > 6_000.0) || local.has_black_water {
            *halophile = self.grow(*halophile, 1.8, 250);
            feedback.halophile = f32::from(*halophile) / f32::from(MICROBE_SCALE);
        } else {
            *halophile = self.decay(2, *halophile);
        }

        let ferro = &mut l1.microbe_ferro[fi];
        if local.metal_cnt > 0 {
            *ferro = self.grow(*ferro, 1.1 * (0.3 + moisture), 230);
            feedback.ferro = f32::from(*ferro) / f32::from(MICROBE_SCALE);
        } else {
            *ferro = self.decay(3, *ferro);
        }
        feedback
    }

    pub(crate) fn replay_tile(
        &self,
        local: &TileConditions,
        l1: &mut FieldChunkSoA,
        fi: usize,
        region_moisture: f32,
    ) {
        let ph = f32::from(l1.ph_milli[fi]) * 0.01;
        let salinity = f32::from(l1.salinity[fi]);
        self.apply(l1, fi, local, region_moisture, ph, salinity);
        local.refresh_conductivity(l1, fi, salinity);
    }
}

impl TerrainChunkSoA {
    pub(crate) fn refresh_replay_conductivity(&mut self, fields: &mut FieldChunkSoA, ds: usize) {
        if self.sleep.flux_mask_dirty {
            self.recompute_flux_mask(fields, ds);
        }
        if self.tile_conditions.is_empty() {
            for (fi, local) in self.tile_conditions(CHUNK_SIZE / ds).iter().enumerate() {
                local.refresh_conductivity(fields, fi, f32::from(fields.salinity[fi]));
            }
            self.sleep.dirty_for_save = true;
        }
    }

    pub(crate) fn tile_conditions(&mut self, axis: usize) -> &[TileConditions] {
        if self.tile_conditions.len() != axis * axis {
            self.tile_conditions.clear();
            for fi in 0..axis * axis {
                let (fx, fy) = (fi % axis, fi / axis);
                let tile = if axis * 2 == CHUNK_SIZE {
                    TileConditions::sample::<2>(self, fx, fy)
                } else {
                    TileConditions::sample::<4>(self, fx, fy)
                };
                self.tile_conditions.push(tile);
            }
        }
        &self.tile_conditions
    }
}
