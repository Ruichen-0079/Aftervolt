use plain_contracts::materials::{FluidKind, MaterialId, MATERIALS};
use plain_contracts::telemetry::EraPhase;
use plain_contracts::{FieldDownsample, CHUNK_SIZE};
use rayon::prelude::*;

use crate::spatial::{
    FieldChunkSoA, TerrainChunkSoA, WorldState, FLUX_BLOCK_E, FLUX_BLOCK_N, FLUX_BLOCK_S,
    FLUX_BLOCK_W,
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
    let resolution = world.config.field_downsample;
    let chunks_x = world.config.dimensions.chunks_x as usize;

    // Ensure any chunk whose L0 solids changed has an up-to-date L1 flux_block_mask.
    for ci in 0..world.l0_chunks.len() {
        if active_mask[ci] && world.l0_chunks[ci].sleep.flux_mask_dirty {
            world.recompute_chunk_flux_mask(ci);
        }
    }

    // Collect read-only region era metadata by chunk column so parallel worker has zero shared mutable borrows.
    let region_params: Vec<(EraPhase, f32)> = (0..chunks_x)
        .map(|cx| {
            let x_mid = cx * CHUNK_SIZE + CHUNK_SIZE / 2;
            let rid = world.region_for_x(x_mid).0 as usize;
            let r = &world.regions[rid.min(world.regions.len().saturating_sub(1))];
            (r.era, r.moisture_index)
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
                let (era, region_moisture) = region_params[cx];
                match resolution {
                    FieldDownsample::By2 => evolve_single_l1_chunk::<2>(
                        l1,
                        l0,
                        cy,
                        world_h,
                        era,
                        region_moisture,
                        &halos[ci],
                        next,
                    ),
                    FieldDownsample::By4 => evolve_single_l1_chunk::<4>(
                        l1,
                        l0,
                        cy,
                        world_h,
                        era,
                        region_moisture,
                        &halos[ci],
                        next,
                    ),
                }
                next.publish(l1);
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
    era: EraPhase,
    region_moisture: f32,
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

        for fx in 0..axis {
            let fi = fy * axis + fx;
            let mask = l1.flux_block_mask[fi];

            // Aggregate local L0 physical conditions for this FieldTile.
            let mut fluid_sum = 0u32;
            let mut solid_cnt = 0u32;
            let mut metal_cnt = 0u32;
            let mut salt_crust_cnt = 0u32;
            let mut heat_core_cnt = 0u32;
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
                        salt_crust_cnt += u32::from(mat == MaterialId::SALT_CRUST);
                        heat_core_cnt += u32::from(mat == MaterialId::RUIN_HEAT_CORE);
                        metal_cnt += u32::from(mat != MaterialId::RUIN_HEAT_CORE && is_metal(mat));
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
            let effective_moisture = wetness.max(region_moisture * 0.35);

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

            // --- 4 Microbial functional guilds (Continuous Ecological Field) ---
            let mut sap = l1.microbe_saprotroph[fi] as f32;
            let mut cond_m = l1.microbe_conductive[fi] as f32;
            let mut halo = l1.microbe_halophile[fi] as f32;
            let mut ferro = l1.microbe_ferro[fi] as f32;

            // 1. Saprotrophic decomposers: thrive in moist, near-neutral, fertile zones; dormant in arid salt desert!
            let ph_ok_sap = (1.0 - ((ph_f - 68.0).abs() / 32.0)).clamp(0.0, 1.0);
            if effective_moisture > 0.20 && ph_ok_sap > 0.25 && sal_f < 38_000.0 {
                let growth = 1.6 * effective_moisture * ph_ok_sap;
                sap = (sap + growth).min(240.0);
                // Fermentation gently warms local soil & recycles fertility while mildly acidifying
                temp_f += sap * 0.015;
                fert_f = (fert_f + sap * 0.45).min(62_000.0);
                ph_f = (ph_f - 0.02).max(52.0);
            } else {
                // Dormancy / die-back during arid alkaline salt-crust phase (creates Core Spirit imbalance conditions)
                sap = (sap * 0.96).max(4.0);
            }

            // 2. Conductive filament mat: prefers mildly acidic & mineral-rich solid surfaces
            if metal_cnt > 0 && ph_f < 72.0 && effective_moisture > 0.12 {
                cond_m = (cond_m + 1.4).min(245.0);
            } else if solid_cnt > 0 && ph_f < 66.0 && effective_moisture > 0.25 {
                cond_m = (cond_m + 0.5).min(180.0);
            } else {
                cond_m = (cond_m * 0.985).max(3.0);
            }

            // 3. Halophile / gas-producing aquatic algae-microbes: thrive in salt marshes, travertine & black water
            if (wetness > 0.15 && sal_f > 6_000.0) || has_black_water {
                halo = (halo + 1.8).min(250.0);
                // Buffers extreme alkalinity slightly and boosts electrolyte availability
                fert_f = (fert_f + halo * 0.25).min(60_000.0);
            } else {
                halo = (halo * 0.97).max(4.0);
            }

            // 4. Ferro-mineralizing microbes: colonize metallic ruins / ore / mineralized bone
            if metal_cnt > 0 {
                ferro = (ferro + 1.1 * (0.3 + effective_moisture)).min(230.0);
                fert_f = (fert_f + ferro * 0.18).min(60_000.0);
            } else {
                ferro = (ferro * 0.98).max(2.0);
            }

            // Salt crust pushes local salinity & alkalinity up
            if salt_crust_cnt > 0 {
                sal_f = (sal_f + salt_crust_cnt as f32 * 45.0).min(64_000.0);
                ph_f = (ph_f + 0.08).min(108.0);
            }

            // Era-level soft local coupling on exposed shallow tiles
            if norm_y > 0.35 && solid_cnt < cell_count_per_tile {
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

            l1.microbe_saprotroph[fi] = sap as u8;
            l1.microbe_conductive[fi] = cond_m as u8;
            l1.microbe_halophile[fi] = halo as u8;
            l1.microbe_ferro[fi] = ferro as u8;

            // Update composite electrical conductivity for L1 electric solver (F4)
            let base_c = cond_accum / cell_count_per_tile.max(1);
            let sal_boost = ((sal_f / 65_535.0) * 320.0 * (0.2 + wetness)) as u32;
            let mic_boost = ((cond_m / 255.0) * 260.0) as u32;
            l1.conductivity[fi] = (base_c + sal_boost + mic_boost).min(1500) as u16;
        }
    }
}
