use plain_contracts::materials::MaterialId;
use plain_contracts::rng::det_sin;
use plain_contracts::CellCoord;
use plain_world::{BasinRole, DarkRiverEdge, SinkholeDescriptor, WorldState};
use rand_core::RngCore;

use crate::noise::fbm2d;

/// Step 4: Carve karst cave networks, continuous deep dark-river aquifer, guaranteed vertical
/// karst sinkholes piercing from surface to deep black water, and giant mineralized fossil cavities.
pub fn carve_underground_and_sinkholes(world: &mut WorldState) {
    let w = world.width_cells();
    let h = world.height_cells();
    let cave_seed = world.seed.derive_u64("karst-caves", 0);
    let warp_seed = world.seed.derive_u64("karst-warp", 1);
    let mut rng = world.seed.derive_rng("underground-features", 2);

    // 1. Mid-depth karst caverns via domain-warped 2D noise (strictly preserving bedrock floor y <= 8
    //    and keeping surface travertine bowls intact).
    for x in 10..(w - 10) {
        let sy = world.topology.surface_height[x] as usize;
        let aqy = world.topology.aquifer_y[x] as usize;
        let cave_min_y = (aqy + 10).min(sy.saturating_sub(14));
        let cave_max_y = sy.saturating_sub(16);
        if cave_max_y <= cave_min_y + 4 {
            continue;
        }
        // Keep vertical world edges solid
        let edge_guard = x < 24 || x > w - 24;
        if edge_guard {
            continue;
        }

        for y in cave_min_y..cave_max_y {
            let wx = x as f64;
            let wy = y as f64;
            let dx = fbm2d(warp_seed, wx * 0.012, wy * 0.012, 2) * 18.0;
            let dy = fbm2d(warp_seed.wrapping_add(99), wx * 0.012, wy * 0.012, 2) * 14.0;
            let n = fbm2d(cave_seed, (wx + dx) * 0.018, (wy + dy) * 0.022, 3);
            if n > 0.36 {
                world.set_solid_unchecked_gen(CellCoord::new(x as i32, y as i32), MaterialId::AIR);
            }
        }
    }

    // 2. Continuous deep underground dark-river aquifer spanning across all basins
    //    Guarantees horizontal underground traversal and deep black-water ecology.
    let river_floor_min = 12usize;
    let dark_seed = world.seed.derive_u64("dark-river", 3);
    for x in 24..(w - 24) {
        let ceil_y = world.topology.aquifer_y[x] as usize;
        let vault_mod = ((fbm2d(dark_seed, x as f64 * 0.015, 0.5, 2) + 1.0) * 10.0) as usize;
        let roof = (ceil_y + 8 + vault_mod).min(world.topology.surface_height[x] as usize - 20);
        let floor = river_floor_min + (vault_mod / 3);
        if roof > floor + 8 {
            for y in floor..roof {
                world.set_solid_unchecked_gen(CellCoord::new(x as i32, y as i32), MaterialId::AIR);
            }
        }
    }

    // Record dark-river graph edges between adjacent L3 basins
    let mut dark_edges = Vec::new();
    for i in 0..world.regions.len().saturating_sub(1) {
        let mid_x = world.regions[i].x_end.min(w - 1);
        let y_lvl = world.topology.aquifer_y[mid_x] as i32;
        dark_edges.push(DarkRiverEdge {
            from_region: world.regions[i].id,
            to_region: world.regions[i + 1].id,
            y_level: y_lvl,
            min_clearance_cells: 12,
        });
    }
    world.topology.dark_river_edges = dark_edges;

    // 3. Carve Vertical Karst Sinkholes (垂直天坑 / 落水洞) directly from open surface down to deep black water
    let mut sinkholes = Vec::new();
    let regions_snapshot = world.regions.clone();
    for (idx, r) in regions_snapshot.iter().enumerate() {
        // Place sinkholes primarily in non-travertine-core spots, always guaranteeing at least 2 world-wide
        let want_sinkhole = r.role != BasinRole::CanyonTravertineTerrace || idx == 0;
        if !want_sinkhole {
            continue;
        }
        let span = r.x_end - r.x_start;
        let margin = (span / 5).max(36).min((span - 1) / 2);
        if r.x_end <= r.x_start + margin * 2 {
            continue;
        }
        let cx = r.x_start + margin + ((rng.next_u32() as usize) % (span - margin * 2).max(1));
        let top_y = (world.topology.surface_height[cx] as i32 + 6).min(h as i32 - 4);
        let bottom_y = (world.topology.aquifer_y[cx] as i32 - 6).max(river_floor_min as i32 + 2);
        let base_radius = (7 + (rng.next_u32() % 6) as i32).min(14);
        let phase = ((rng.next_u32() % 1000) as f64) * 0.01;

        for y in bottom_y..=top_y {
            let t = (y - bottom_y) as f64 / (top_y - bottom_y).max(1) as f64;
            // Gentle natural meandering shaft (keeps shaft cleanly traversable & visually organic)
            let offset_x = (det_sin(t * 7.5 + phase) * 8.0) as i32;
            let flare = if t > 0.88 {
                ((t - 0.88) * 55.0) as i32
            } else {
                0
            };
            let rad = base_radius + flare;
            let shaft_cx = (cx as i32 + offset_x).clamp(28, (w as i32) - 29);

            for dx in -rad..=rad {
                world.set_solid_unchecked_gen(CellCoord::new(shaft_cx + dx, y), MaterialId::AIR);
            }
        }

        sinkholes.push(SinkholeDescriptor {
            region_id: r.id,
            x_center: cx as i32,
            top_y,
            bottom_y,
            mean_radius: base_radius as u16,
        });
    }
    world.topology.sinkholes = sinkholes;

    // 4. Embed Giant Mineralized Fossil Cavities (巨型矿化古生物遗骸空腔)
    //    Creates arched mineralized-bone ribcages with hollow navigable interiors.
    let fossil_count = (w / 1800).clamp(2, 5);
    let mut fossils = Vec::with_capacity(fossil_count);
    for k in 0..fossil_count {
        let fx = (((k + 1) * w) / (fossil_count + 1)) as i32 + ((rng.next_u32() % 120) as i32 - 60);
        let fx = fx.clamp(90, (w as i32) - 91);
        let sy = world.topology.surface_height[fx as usize] as i32;
        let aqy = world.topology.aquifer_y[fx as usize] as i32;
        let fy_min = aqy + 28;
        let fy_max = sy - 26;
        let rx = (22 + (rng.next_u32() % 14) as i32).min(38);
        let mut ry = (14 + (rng.next_u32() % 9) as i32).min(24);
        let midpoint = (sy + aqy) / 2;
        let fy = if fy_min <= fy_max {
            midpoint.clamp(fy_min, fy_max)
        } else {
            ry = ry.min((sy - aqy) / 2 - 4);
            if ry < 1 {
                continue;
            }
            midpoint
        };

        for dy in -(ry + 3)..=(ry + 3) {
            for dx in -(rx + 3)..=(rx + 3) {
                let nx = (dx as f64) / (rx as f64);
                let ny = (dy as f64) / (ry as f64);
                let dist2 = nx * nx + ny * ny;
                let cell = CellCoord::new(fx + dx, fy + dy);
                if dist2 < 0.78 {
                    // Hollow interior chamber
                    world.set_solid_unchecked_gen(cell, MaterialId::AIR);
                } else if dist2 <= 1.12 {
                    // Ribbed mineralized bone shell with openings between ribs
                    if ((dx.abs() % 7) <= 3) || dy < 0 {
                        if !is_open_sinkhole_cell(world, cell) {
                            world.set_solid_unchecked_gen(cell, MaterialId::MINERALIZED_BONE);
                        }
                    } else {
                        world.set_solid_unchecked_gen(cell, MaterialId::AIR);
                    }
                }
            }
        }
        fossils.push((CellCoord::new(fx, fy), rx as u16));
    }
    world.topology.fossil_cavities = fossils;
}

/// Later worldgen features must leave an already carved shaft open.
pub(crate) fn is_open_sinkhole_cell(world: &WorldState, cell: CellCoord) -> bool {
    world.topology.sinkholes.iter().any(|shaft| {
        cell.y >= shaft.bottom_y
            && cell.y <= shaft.top_y
            && cell.x.abs_diff(shaft.x_center) <= u32::from(shaft.mean_radius) + 15
            && world.get_material(cell).is_air()
    })
}
