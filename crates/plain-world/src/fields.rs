use plain_contracts::materials::{FluidKind, MaterialId, MATERIALS};
use plain_contracts::telemetry::EraPhase;
use plain_contracts::CHUNK_SIZE;
use rayon::prelude::*;

use crate::spatial::{
    FieldChunkSoA, TerrainChunkSoA, WorldState, FLUX_BLOCK_E, FLUX_BLOCK_N, FLUX_BLOCK_S,
    FLUX_BLOCK_W,
};

/// Step wall-aware L1 physicochemical (pH, fertility, salinity, temperature) & 4 microbial fields
/// across all chunks marked in `active_mask`.
///
/// Strictly enforces `flux_block_mask` so heat, pH, salinity, and microbes NEVER leak across
/// thin impermeable rock or ruin walls (F4).
pub fn step_l1_fields(world: &mut WorldState, active_mask: &[bool]) {
    let ds = world.config.field_downsample.factor();
    let axis = world.config.field_downsample.tiles_per_chunk_axis();
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

    // Parallel intra-chunk wall-aware field evolution & microbial dynamics.
    world
        .l1_chunks
        .par_iter_mut()
        .zip(world.l0_chunks.par_iter_mut())
        .enumerate()
        .for_each(|(ci, (l1, l0))| {
            if !active_mask[ci] {
                return;
            }
            let cx = ci % chunks_x;
            let cy = ci / chunks_x;
            let (era, region_moisture) = region_params[cx];
            evolve_single_l1_chunk(l1, l0, cy, world_h, ds, axis, era, region_moisture);
            l0.sleep.dirty_for_save = true;
        });
}

#[allow(clippy::too_many_arguments)]
fn evolve_single_l1_chunk(
    l1: &mut FieldChunkSoA,
    l0: &TerrainChunkSoA,
    cy: usize,
    world_h: usize,
    ds: usize,
    axis: usize,
    era: EraPhase,
    region_moisture: f32,
) {
    let n = axis * axis;
    // Scratch buffers for deterministic wall-aware diffusion inside this chunk (stack/small heap per thread).
    let mut next_ph = l1.ph_scaled.clone();
    let mut next_temp = l1.temperature_dk.clone();
    let mut next_fert = l1.fertility.clone();
    let mut next_sal = l1.salinity.clone();

    let cell_count_per_tile = (ds * ds) as u32;

    for fy in 0..axis {
        let world_y = cy * CHUNK_SIZE + fy * ds + ds / 2;
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

            let x0 = fx * ds;
            let y0 = fy * ds;
            for dy in 0..ds {
                let row = (y0 + dy) * CHUNK_SIZE + x0;
                for dx in 0..ds {
                    let li = row + dx;
                    let mat = MaterialId(l0.solid_material[li]);
                    let def = MATERIALS.get(mat);
                    if mat.is_solid() {
                        solid_cnt += 1;
                        cond_accum += def.conductivity as u32;
                        if mat == MaterialId::SALT_CRUST {
                            salt_crust_cnt += 1;
                        } else if mat == MaterialId::RUIN_HEAT_CORE {
                            heat_core_cnt += 1;
                        } else if mat.is_ruin_material()
                            || mat == MaterialId::CONDUCTIVE_ORE
                            || mat == MaterialId::MINERALIZED_BONE
                        {
                            metal_cnt += 1;
                        }
                    } else {
                        let fvol = l0.fluid_volume[li] as u32;
                        let fkind = FluidKind::from_u8(l0.fluid_kind[li]);
                        fluid_sum += fvol;
                        if fkind == FluidKind::BlackWater && fvol > 1000 {
                            has_black_water = true;
                        }
                        if fvol > 0 {
                            cond_accum += (fkind.base_conductivity() as u32 * fvol) / 10_000;
                        } else {
                            cond_accum += def.conductivity as u32;
                        }
                    }
                }
            }

            let wetness = ((fluid_sum as f32) / (cell_count_per_tile as f32 * 10_000.0)).min(1.0);
            let effective_moisture = wetness.max(region_moisture * 0.35);

            // --- Wall-aware 4-neighbor diffusion (zero leakage across blocked walls!) ---
            let mut ph_acc = l1.ph_scaled[fi] as f32 * 4.0;
            let mut temp_acc = l1.temperature_dk[fi] as f32 * 4.0;
            let mut fert_acc = l1.fertility[fi] as f32 * 4.0;
            let mut sal_acc = l1.salinity[fi] as f32 * 4.0;
            let mut weight = 4.0f32;

            // North
            if fy + 1 < axis && (mask & FLUX_BLOCK_N) == 0 {
                let nfi = (fy + 1) * axis + fx;
                if (l1.flux_block_mask[nfi] & FLUX_BLOCK_S) == 0 {
                    ph_acc += l1.ph_scaled[nfi] as f32;
                    temp_acc += l1.temperature_dk[nfi] as f32;
                    fert_acc += l1.fertility[nfi] as f32;
                    sal_acc += l1.salinity[nfi] as f32;
                    weight += 1.0;
                }
            }
            // South
            if fy > 0 && (mask & FLUX_BLOCK_S) == 0 {
                let sfi = (fy - 1) * axis + fx;
                if (l1.flux_block_mask[sfi] & FLUX_BLOCK_N) == 0 {
                    ph_acc += l1.ph_scaled[sfi] as f32;
                    temp_acc += l1.temperature_dk[sfi] as f32;
                    fert_acc += l1.fertility[sfi] as f32;
                    sal_acc += l1.salinity[sfi] as f32;
                    weight += 1.0;
                }
            }
            // East
            if fx + 1 < axis && (mask & FLUX_BLOCK_E) == 0 {
                let efi = fy * axis + (fx + 1);
                if (l1.flux_block_mask[efi] & FLUX_BLOCK_W) == 0 {
                    ph_acc += l1.ph_scaled[efi] as f32;
                    temp_acc += l1.temperature_dk[efi] as f32;
                    fert_acc += l1.fertility[efi] as f32;
                    sal_acc += l1.salinity[efi] as f32;
                    weight += 1.0;
                }
            }
            // West
            if fx > 0 && (mask & FLUX_BLOCK_W) == 0 {
                let wfi = fy * axis + (fx - 1);
                if (l1.flux_block_mask[wfi] & FLUX_BLOCK_E) == 0 {
                    ph_acc += l1.ph_scaled[wfi] as f32;
                    temp_acc += l1.temperature_dk[wfi] as f32;
                    fert_acc += l1.fertility[wfi] as f32;
                    sal_acc += l1.salinity[wfi] as f32;
                    weight += 1.0;
                }
            }

            let mut ph_f = ph_acc / weight;
            let mut temp_f = temp_acc / weight;
            let mut fert_f = fert_acc / weight;
            let mut sal_f = sal_acc / weight;

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

            next_ph[fi] = ph_f.clamp(25.0, 125.0) as u8;
            next_temp[fi] = temp_f.clamp(2200.0, 4200.0) as u16;
            next_fert[fi] = fert_f.clamp(0.0, 65_535.0) as u16;
            next_sal[fi] = sal_f.clamp(0.0, 65_535.0) as u16;

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

    debug_assert_eq!(next_ph.len(), n);
    l1.ph_scaled = next_ph;
    l1.temperature_dk = next_temp;
    l1.fertility = next_fert;
    l1.salinity = next_sal;
}
