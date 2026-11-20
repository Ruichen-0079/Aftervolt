use plain_contracts::materials::MaterialId;
use plain_contracts::CellCoord;
use plain_world::spatial::CELL_FLAG_FLOOD_CHANNEL;
use plain_world::{BasinRole, WorldState};
use rand_core::RngCore;

use crate::noise::{fbm1d, fbm2d, ridged1d, smoothstep};

/// Step 3: Sculpt staggered 2D surface relief, hydraulic erosion, stepped travertine terraces,
/// seasonal flood wadis, hanging stone pillars & wind-eroded arches, and solid geological strata.
pub fn sculpt_surface_and_strata(world: &mut WorldState) {
    let w = world.width_cells();
    let h = world.height_cells();
    let h_f = h as f64;

    let warp_seed = world.seed.derive_u64("surface-warp", 0);
    let ridge_seed = world.seed.derive_u64("surface-ridge", 1);
    let detail_seed = world.seed.derive_u64("surface-detail", 2);
    let strata_seed = world.seed.derive_u64("strata-noise", 3);
    let mut rng = world.seed.derive_rng("surface-features", 4);

    // 1. Continuous basin-blended target surface profile with domain warping & ridged highlands
    let mut height_f = vec![0.0f64; w];
    let blend_half = ((w as f64) * 0.035).clamp(28.0, 140.0);

    for (x, hf) in height_f.iter_mut().enumerate() {
        let xf = x as f64;
        // Blend smoothly across adjacent L3 basins
        let mut target_y = 0.0;
        let mut weight_sum = 0.0;
        let mut ridge_weight = 0.0;
        let mut basin_flat_weight = 0.0;

        for r in &world.regions {
            let left_fade = smoothstep(r.x_start as f64 - blend_half, r.x_start as f64 + blend_half, xf);
            let right_fade = 1.0 - smoothstep(r.x_end as f64 - blend_half, r.x_end as f64 + blend_half, xf);
            let wt = (left_fade * right_fade).max(0.0);
            if wt > 0.0001 {
                target_y += (r.mean_surface_y as f64) * wt;
                weight_sum += wt;
                if r.role == BasinRole::RidgePlateau {
                    ridge_weight += wt;
                } else if r.role == BasinRole::ElectrolyteSaltBasin {
                    basin_flat_weight += wt;
                }
            }
        }
        let base_y = if weight_sum > 0.0 {
            target_y / weight_sum
        } else {
            h_f * 0.55
        };

        // Finite world left/right boundary escarpments (natural geological confinement)
        let edge_wall = (1.0 - smoothstep(0.0, (w as f64) * 0.025, xf))
            + smoothstep((w as f64) * 0.975, w as f64, xf);
        let boundary_lift = edge_wall * h_f * 0.16;

        // Domain-warped fBm + sharp highland ridges vs flatter salt-basin floors
        let warp = fbm1d(warp_seed, xf * 0.0022, 3) * 70.0;
        let broad = fbm1d(detail_seed, (xf + warp) * 0.0032, 4);
        let ridged = ridged1d(ridge_seed, (xf + warp * 0.6) * 0.0045, 4);
        let fine = fbm1d(detail_seed.wrapping_add(77), xf * 0.024, 3);

        let ridge_amp = (ridge_weight / weight_sum.max(0.01)) * h_f * 0.135;
        let flat_damp = 1.0 - 0.58 * (basin_flat_weight / weight_sum.max(0.01));

        let y = base_y
            + boundary_lift
            + broad * h_f * 0.065 * flat_damp
            + (ridged - 0.35) * ridge_amp
            + fine * h_f * 0.014 * flat_damp;

        *hf = y.clamp(h_f * 0.30, h_f * 0.91);
    }

    // 2. Deterministic 1D hydraulic erosion & hillslope diffusion (inspired by Veloren)
    //    Carves realistic gullies and deposits smooth sediment aprons in lowlands.
    let mut sediment = vec![0.0f64; w];
    for pass in 0..18 {
        let mut next_h = height_f.clone();
        for x in 2..(w - 2) {
            let slope_l = height_f[x] - height_f[x - 1];
            let slope_r = height_f[x] - height_f[x + 1];
            let max_slope = slope_l.max(slope_r);
            if max_slope > 0.35 {
                let erode = (max_slope - 0.35) * 0.18;
                next_h[x] -= erode;
                if slope_l >= slope_r {
                    sediment[x - 1] += erode * 0.85;
                } else {
                    sediment[x + 1] += erode * 0.85;
                }
            }
            // Deposit carried sediment in gentler basins
            if sediment[x] > 0.01 && max_slope < 0.28 {
                let dep = sediment[x] * 0.5;
                next_h[x] += dep;
                sediment[x] -= dep;
            }
            // Hillslope thermal diffusion every 3rd pass for natural talus bases
            if pass % 3 == 0 {
                let avg = 0.25 * height_f[x - 1] + 0.5 * height_f[x] + 0.25 * height_f[x + 1];
                next_h[x] = next_h[x] * 0.82 + avg * 0.18;
            }
        }
        height_f = next_h;
    }

    // 3. Carve seasonal flood wadis (洪泛古河床) into canyon / fault slopes
    let mut flood_wadis = Vec::new();
    for r in &world.regions {
        if matches!(r.role, BasinRole::CanyonTravertineTerrace | BasinRole::FaultBasin) {
            let span = r.x_end - r.x_start;
            let cx = r.x_start + span / 3 + ((rng.next_u32() as usize) % (span / 3).max(1));
            let wadi_half_w = ((w as f64) * 0.008).clamp(10.0, 32.0) as usize;
            let wadi_depth = (h_f * 0.032).clamp(12.0, 48.0);
            for dx in -(wadi_half_w as isize)..=(wadi_half_w as isize) {
                let xx = (cx as isize + dx).clamp(4, (w - 5) as isize) as usize;
                let u = 1.0 - (dx.abs() as f64 / (wadi_half_w as f64 + 1.0));
                height_f[xx] = (height_f[xx] - wadi_depth * u * u).max(h_f * 0.27);
            }
            flood_wadis.push(cx as i32);
        }
    }

    // Store integer surface profile
    for x in 0..w {
        world.topology.surface_height[x] = height_f[x].clamp(h_f * 0.26, h_f * 0.92) as u16;
    }
    world.topology.flood_wadis = flood_wadis.clone();

    // 4. Fill solid geological strata into L0 chunks
    let ore_seed = world.seed.derive_u64("ore-veins", 5);
    for x in 0..w {
        let sy = world.topology.surface_height[x] as usize;
        let rid = world.region_for_x(x).0 as usize;
        let role = world.regions[rid].role;
        let era = world.regions[rid].era;

        let strata_shift = (fbm1d(strata_seed, x as f64 * 0.007, 3) * (h_f * 0.04)) as isize;
        let bedrock_top = (((h as f64) * 0.10) as isize + strata_shift / 2).clamp(10, (h / 4) as isize) as usize;
        let granite_top = ((sy as isize) - ((h as f64) * 0.14) as isize + strata_shift)
            .clamp(bedrock_top as isize + 8, sy as isize) as usize;

        for y in 0..=sy {
            let mat = if y <= bedrock_top {
                MaterialId::BASALT_BEDROCK
            } else if y <= granite_top {
                let ore_n = fbm2d(ore_seed, x as f64 * 0.045, y as f64 * 0.045, 2);
                if ore_n > 0.56 {
                    MaterialId::CONDUCTIVE_ORE
                } else {
                    MaterialId::GRANITE
                }
            } else if y + 5 >= sy {
                // Topsoil / basin crust varies naturally by geological role & current era
                match role {
                    BasinRole::ElectrolyteSaltBasin => {
                        if matches!(
                            era,
                            plain_contracts::EraPhase::SaltCrustDesert
                                | plain_contracts::EraPhase::SilentReverseRain
                        ) && y + 2 >= sy
                        {
                            MaterialId::SALT_CRUST
                        } else {
                            MaterialId::SILT_CLAY
                        }
                    }
                    BasinRole::CanyonTravertineTerrace => MaterialId::TRAVERTINE,
                    BasinRole::FaultBasin => {
                        if y + 3 >= sy {
                            MaterialId::LOOSE_SOIL_DAM
                        } else {
                            MaterialId::SANDSTONE
                        }
                    }
                    BasinRole::RidgePlateau => MaterialId::GRANITE,
                }
            } else {
                MaterialId::SANDSTONE
            };

            world.set_solid_unchecked_gen(CellCoord::new(x as i32, y as i32), mat);
        }

        // Tag flood wadi channels
        for &wx in &flood_wadis {
            if (x as i32 - wx).abs() <= 14 && sy + 4 < h {
                for dy in 0..4 {
                    world.set_cell_flag(CellCoord::new(x as i32, (sy + dy) as i32), CELL_FLAG_FLOOD_CHANNEL);
                }
            }
        }
    }

    // 5. Sculpt Stepped Travertine Terraces (阶梯钙华池) on canyon/terrace slopes
    //    Raises calcified Travertine rims holding tiered concave basins.
    let mut travertine_zones = Vec::new();
    for r in world.regions.clone() {
        if r.role == BasinRole::CanyonTravertineTerrace {
            let span = r.x_end - r.x_start;
            let t_start = r.x_start + span / 5;
            let t_end = (r.x_end - span / 5).min(w - 16);
            if t_end > t_start + 60 {
                travertine_zones.push((t_start, t_end));
                let step_w = ((t_end - t_start) / 5).clamp(18, 64);
                let mut sx = t_start;
                while sx + step_w <= t_end {
                    // Terrace level anchored to lower end of step
                    let y_left = world.topology.surface_height[sx] as i32;
                    let y_right = world.topology.surface_height[sx + step_w - 1] as i32;
                    let rim_y = y_left.min(y_right) + 5;
                    let floor_y = (rim_y - 7).max(20);

                    for x in sx..(sx + step_w) {
                        let is_rim = x < sx + 3 || x >= sx + step_w - 3;
                        let target_solid_top = if is_rim { rim_y } else { floor_y };
                        let cur_top = world.topology.surface_height[x] as i32;
                        if cur_top > target_solid_top {
                            // Carve flat basin bowl
                            for y in (target_solid_top + 1)..=cur_top {
                                world.set_solid_unchecked_gen(CellCoord::new(x as i32, y), MaterialId::AIR);
                            }
                        } else {
                            // Build calcified Travertine support rim
                            for y in cur_top.max(10)..=target_solid_top {
                                world.set_solid_unchecked_gen(CellCoord::new(x as i32, y), MaterialId::TRAVERTINE);
                            }
                        }
                        // Line floor with Travertine so pool retains tiered water
                        for y in (floor_y - 3).max(8)..=target_solid_top {
                            world.set_solid_unchecked_gen(CellCoord::new(x as i32, y), MaterialId::TRAVERTINE);
                        }
                        world.topology.surface_height[x] = target_solid_top as u16;
                    }
                    sx += step_w;
                }
            }
        }
    }
    world.topology.travertine_zones = travertine_zones;

    // 6. Sculpt Hanging Stone Pillars & Wind-Eroded Arches (悬空石林与风蚀拱桥)
    //    Adds true vertical overhang architecture above highlands & fault valleys.
    for r in world.regions.clone() {
        if matches!(r.role, BasinRole::RidgePlateau | BasinRole::FaultBasin) {
            let span = r.x_end - r.x_start;
            let arch_count = (span / 380).clamp(1, 3);
            for k in 0..arch_count {
                let cx = r.x_start + ((k + 1) * span) / (arch_count + 1);
                if cx < 60 || cx + 60 >= w {
                    continue;
                }
                let half_span = (18 + (rng.next_u32() % 18) as usize).min(40);
                let arch_height = (20 + (rng.next_u32() % 18) as usize).min(46);
                let thickness = 6 + (rng.next_u32() % 5) as usize;
                let left_y = world.topology.surface_height[cx - half_span] as i32;
                let right_y = world.topology.surface_height[cx + half_span] as i32;
                let base_y = left_y.min(right_y);
                let crown_y = (base_y + arch_height as i32).min(h as i32 - 20);

                // Build left & right eroded stone pillars
                for &px in &[cx - half_span, cx + half_span] {
                    for dx in -4i32..=4i32 {
                        let xx = px as i32 + dx;
                        let ground = world.topology.surface_height[xx as usize] as i32;
                        for y in (ground - 4).max(12)..=(base_y + arch_height as i32 / 2) {
                            world.set_solid_unchecked_gen(CellCoord::new(xx, y), MaterialId::SANDSTONE);
                        }
                    }
                }

                // Span overhanging arch deck with open air passage underneath
                for dx in -(half_span as i32)..=(half_span as i32) {
                    let u = (dx as f64) / (half_span as f64);
                    let curve_y = base_y + ((1.0 - u * u) * (crown_y - base_y) as f64) as i32;
                    let xx = (cx as i32) + dx;
                    for dy in 0..(thickness as i32) {
                        let yy = curve_y + dy;
                        if yy < h as i32 - 8 {
                            world.set_solid_unchecked_gen(CellCoord::new(xx, yy), MaterialId::SANDSTONE);
                        }
                    }
                }
            }
        }
    }
}
