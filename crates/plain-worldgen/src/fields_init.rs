use plain_contracts::materials::{FluidKind, MaterialId, MATERIALS};
use plain_contracts::{CellCoord, CHUNK_SIZE};
use plain_world::spatial::{CELL_FLAG_DORMANT_BANK, CELL_FLAG_SUNLIT};
use plain_world::{electric, fields, BasinRole, WorldState};

/// Step 7: Compute L0-derived wall-aware L1 `flux_block_mask`, initialize L1 physicochemical
/// fields (pH, fertility, salinity, temperature) & 4 microbial guilds, tag dormant desert seed/cocoon
/// banks, measure natural Electrolyte Flower Sea suitability, and run initial electric field solve.
pub fn initialize_fields_and_climate(world: &mut WorldState) {
    let w = world.width_cells();
    let h = world.height_cells();
    let ds = world.config.field_downsample.factor();
    let axis = world.config.field_downsample.tiles_per_chunk_axis();
    let chunks_x = world.config.dimensions.chunks_x as usize;

    // 1. Mark sunlit surface cells & dormant desert seed/mud-cocoon banks on exposed lowland/wadi soil
    let mut natural_flower_cols = 0usize;
    for x in 0..w {
        let sy = world.topology.surface_height[x] as usize;
        let rid = world.region_for_x(x).0 as usize;
        let role = world.regions[rid].role;

        for y in sy.saturating_sub(2)..h {
            world.set_cell_flag(CellCoord::new(x as i32, y as i32), CELL_FLAG_SUNLIT);
        }

        if matches!(
            role,
            BasinRole::ElectrolyteSaltBasin | BasinRole::FaultBasin | BasinRole::CanyonTravertineTerrace
        ) {
            for dy in 0..4 {
                let yy = sy.saturating_sub(dy);
                world.set_cell_flag(CellCoord::new(x as i32, yy as i32), CELL_FLAG_DORMANT_BANK);
            }
        }

        // Count natural Electrolyte Flower Sea suitable surface columns (strictly excluding ruin anchors!)
        let surface_mat = world.get_material(CellCoord::new(x as i32, sy as i32));
        if role == BasinRole::ElectrolyteSaltBasin && !surface_mat.is_ruin_material() {
            natural_flower_cols += 1;
        }
    }
    world.topology.natural_flower_sea_column_ratio =
        (natural_flower_cols as f32) / (w.max(1) as f32);

    // 2. Compute L0-derived wall-aware flux masks across all chunks (F4)
    world.recompute_all_flux_masks();

    // 3. Initialize L1 physicochemical & 4 microbial fields from L0 geology, hydrology & L3 basins
    for ci in 0..world.l1_chunks.len() {
        let cx = ci % chunks_x;
        let cy = ci / chunks_x;
        let x_origin = cx * CHUNK_SIZE;
        let y_origin = cy * CHUNK_SIZE;
        let rid = world.region_for_x(x_origin + CHUNK_SIZE / 2).0 as usize;
        let role = world.regions[rid].role;
        let r_moist = world.regions[rid].moisture_index;
        let r_fert = world.regions[rid].fertility_index;
        let r_salt = world.regions[rid].salt_crust_ratio;

        let l0 = &world.l0_chunks[ci];
        let l1 = &mut world.l1_chunks[ci];

        for fy in 0..axis {
            let wy = y_origin + fy * ds + ds / 2;
            let norm_y = (wy as f32) / (h.max(1) as f32);

            for fx in 0..axis {
                let fi = fy * axis + fx;
                let x0 = fx * ds;
                let y0 = fy * ds;

                let mut fluid_cnt = 0u32;
                let mut salt_water_cnt = 0u32;
                let mut black_water_cnt = 0u32;
                let mut travertine_cnt = 0u32;
                let mut metal_cnt = 0u32;
                let mut heat_core_cnt = 0u32;
                let mut salt_crust_cnt = 0u32;
                let mut cond_sum = 0u32;
                let n_cells = (ds * ds) as u32;

                for dy in 0..ds {
                    let row = (y0 + dy) * CHUNK_SIZE + x0;
                    for dx in 0..ds {
                        let li = row + dx;
                        let mat = MaterialId(l0.solid_material[li]);
                        if mat.is_solid() {
                            cond_sum += MATERIALS.get(mat).conductivity as u32;
                            match mat {
                                MaterialId::SALT_CRUST => salt_crust_cnt += 1,
                                MaterialId::TRAVERTINE => travertine_cnt += 1,
                                MaterialId::RUIN_HEAT_CORE => heat_core_cnt += 1,
                                m if m.is_ruin_material()
                                    || m == MaterialId::CONDUCTIVE_ORE
                                    || m == MaterialId::MINERALIZED_BONE =>
                                {
                                    metal_cnt += 1;
                                }
                                _ => {}
                            }
                        } else {
                            let fv = l0.fluid_volume[li];
                            if fv > 500 {
                                fluid_cnt += 1;
                                let fk = FluidKind::from_u8(l0.fluid_kind[li]);
                                cond_sum += fk.base_conductivity() as u32;
                                match fk {
                                    FluidKind::SaltMarsh => salt_water_cnt += 1,
                                    FluidKind::BlackWater => black_water_cnt += 1,
                                    FluidKind::TravertinePool => travertine_cnt += 1,
                                    _ => {}
                                }
                            } else {
                                cond_sum += 5;
                            }
                        }
                    }
                }

                // Initial pH (70 = neutral)
                let mut ph = 70.0f32;
                if salt_crust_cnt > 0 || salt_water_cnt > 0 {
                    ph += 14.0 * r_salt + 6.0;
                } else if travertine_cnt > 0 {
                    ph += 7.0;
                } else if black_water_cnt > 0 {
                    ph -= 11.0; // Mildly acidic industrial black water
                } else if role == BasinRole::RidgePlateau {
                    ph -= 6.0;
                }
                l1.ph_milli[fi] = (ph.clamp(35.0, 112.0) * 100.0).round() as u16;

                // Initial fertility (ElectrolyteSaltBasin naturally rich in mineral electrolytes)
                let base_fert = match role {
                    BasinRole::ElectrolyteSaltBasin => 26_000.0 + r_fert * 28_000.0,
                    BasinRole::CanyonTravertineTerrace => 18_000.0 + r_fert * 16_000.0,
                    BasinRole::FaultBasin => 12_000.0 + r_fert * 14_000.0,
                    BasinRole::RidgePlateau => 7_000.0 + r_fert * 8_000.0,
                };
                let fert = if black_water_cnt > 0 {
                    base_fert.max(30_000.0)
                } else {
                    base_fert
                };
                l1.fertility[fi] = fert.clamp(500.0, 62_000.0) as u16;

                // Initial salinity
                let mut sal = match role {
                    BasinRole::ElectrolyteSaltBasin => 16_000.0 + r_salt * 32_000.0,
                    BasinRole::CanyonTravertineTerrace => 6_000.0 + (travertine_cnt as f32) * 800.0,
                    _ => 2_500.0 + r_salt * 6_000.0,
                };
                if salt_water_cnt > 0 || salt_crust_cnt > 0 {
                    sal = (sal + 14_000.0).min(62_000.0);
                }
                l1.salinity[fi] = sal.clamp(200.0, 64_000.0) as u16;

                // Initial temperature (altitude lapse + deep geothermal + RuinHeatCore bloom)
                let base_temp = if norm_y < 0.22 {
                    2975.0 + (0.22 - norm_y) * 420.0
                } else {
                    2960.0 - (norm_y - 0.22) * 200.0
                };
                let temp = base_temp + (heat_core_cnt as f32) * 190.0;
                l1.temperature_dk[fi] = temp.clamp(2500.0, 3900.0) as u16;

                // Initial 4 microbial continuous fields
                l1.microbe_saprotroph[fi] = if r_moist > 0.35 && salt_crust_cnt == 0 {
                    (40.0 + r_fert * 120.0) as u8
                } else {
                    12
                };
                l1.microbe_conductive[fi] = if metal_cnt > 0 {
                    (65.0 + r_moist * 90.0) as u8
                } else {
                    14
                };
                l1.microbe_halophile[fi] = if salt_water_cnt > 0 || black_water_cnt > 0 {
                    (90.0 + (fluid_cnt as f32 / n_cells as f32) * 110.0).min(240.0) as u8
                } else {
                    10
                };
                l1.microbe_ferro[fi] = if metal_cnt > 0 { 95 } else { 8 };

                l1.conductivity[fi] = ((cond_sum / n_cells.max(1))
                    + ((sal / 65_535.0) * 260.0) as u32
                    + (l1.microbe_conductive[fi] as u32))
                    .min(1500) as u16;
            }
        }
    }

    // 4. Run 2 relaxation sweeps of L1 fields and 1 pass of local electric field so Tick 0 is warm
    let all_active = vec![true; world.l0_chunks.len()];
    fields::step_l1_fields(world, &all_active);
    electric::solve_local_electric_field(world, &all_active);
}
