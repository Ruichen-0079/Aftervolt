use std::collections::VecDeque;

use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::CellCoord;
use plain_world::{RuinAnchorKind, WorldState};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldValidationReport {
    pub passed: bool,
    pub checks: Vec<ValidationCheck>,
}

impl WorldValidationReport {
    pub fn failure_summary(&self) -> String {
        self.checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| format!("{}: {}", c.name, c.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Automated 8-gate world legality verification (Stage 1 DoD: 100 random seeds 100% pass).
pub fn validate_world(world: &WorldState) -> WorldValidationReport {
    let mut checks = Vec::with_capacity(8);
    let w = world.width_cells();
    let h = world.height_cells();

    // Gate 1: Finite bedrock floor seal (y = 0 must be solid across full width)
    let mut floor_breaches = 0usize;
    for x in 0..w {
        if world.get_material(CellCoord::new(x as i32, 0)).is_air() {
            floor_breaches += 1;
        }
    }
    checks.push(ValidationCheck {
        name: "finite_bounds_intact".into(),
        passed: floor_breaches == 0 && w >= 512 && h >= 384,
        detail: format!("width={w}, height={h}, floor_breaches={floor_breaches}"),
    });

    // Gate 2: Topographic relief contrast (guarantees staggered highlands vs lowlands)
    let min_sy = world
        .topology
        .surface_height
        .iter()
        .copied()
        .min()
        .unwrap_or(0);
    let max_sy = world
        .topology
        .surface_height
        .iter()
        .copied()
        .max()
        .unwrap_or(0);
    let relief_frac = ((max_sy.saturating_sub(min_sy)) as f32) / (h.max(1) as f32);
    checks.push(ValidationCheck {
        name: "surface_relief_contrast".into(),
        passed: relief_frac >= 0.18,
        detail: format!(
            "min_y={min_sy}, max_y={max_sy}, relief={:.1}%",
            relief_frac * 100.0
        ),
    });

    // Gate 3: Expansive natural Electrolyte Flower Sea habitat (strictly ruin-independent)
    let flower_ratio = world.topology.natural_flower_sea_column_ratio;
    checks.push(ValidationCheck {
        name: "natural_flower_sea_habitat".into(),
        passed: flower_ratio >= 0.14,
        detail: format!("natural_flower_sea_columns={:.1}%", flower_ratio * 100.0),
    });

    // Gate 4: Deep BlackWater aquifer & horizontal span
    let mut bw_cells = 0u64;
    let mut travertine_fluid_cells = 0u64;
    let mut salt_fluid_cells = 0u64;
    for ch in &world.l0_chunks {
        for i in 0..ch.fluid_volume.len() {
            if ch.fluid_volume[i] > 0 {
                match FluidKind::from_u8(ch.fluid_kind[i]) {
                    FluidKind::BlackWater => bw_cells += 1,
                    FluidKind::TravertinePool => travertine_fluid_cells += 1,
                    FluidKind::SaltMarsh => salt_fluid_cells += 1,
                    _ => {}
                }
            }
        }
    }
    checks.push(ValidationCheck {
        name: "deep_black_water_aquifer".into(),
        passed: bw_cells >= (w as u64) * 8 && !world.topology.dark_river_edges.is_empty(),
        detail: format!(
            "black_water_cells={bw_cells}, salt_marsh_cells={salt_fluid_cells}, travertine_pool_cells={travertine_fluid_cells}"
        ),
    });

    // Gate 5: Exact four-neighbor BFS confirming open/water connectivity from surface down sinkhole to deep BlackWater
    let sinkhole_connected = verify_surface_to_blackwater_bfs(world);
    checks.push(ValidationCheck {
        name: "sinkhole_to_blackwater_connectivity".into(),
        passed: sinkhole_connected && !world.topology.sinkholes.is_empty(),
        detail: format!(
            "sinkholes={}, bfs_reached_blackwater={sinkhole_connected}",
            world.topology.sinkholes.len()
        ),
    });

    // Gate 6: Scattered modest ruin ratio (~90% natural / ~10% max ruin ceiling) & DeepRuinCore present
    let ruin_ratio = (world.topology.initial_ruin_solid_cells as f64)
        / (world.topology.initial_total_solid_cells.max(1) as f64);
    let has_core = world
        .topology
        .ruin_anchors
        .iter()
        .any(|a| a.kind == RuinAnchorKind::DeepRuinCore);
    checks.push(ValidationCheck {
        name: "ruin_ratio_and_core_anchor".into(),
        passed: has_core && ruin_ratio >= 0.0002 && ruin_ratio <= 0.12,
        detail: format!(
            "anchors={}, ruin_solid_ratio={:.3}%, has_deep_core={has_core}",
            world.topology.ruin_anchors.len(),
            ruin_ratio * 100.0
        ),
    });

    // Gate 7: Hydrology conservation baseline & positive atmospheric/surface water
    let (surf_w, vapor_w, total_w) = world.total_world_water();
    let water_ok = surf_w.0 > 0 && vapor_w.0 > 0 && total_w.0 == world.initial_world_water_ul;
    checks.push(ValidationCheck {
        name: "hydrology_conservation_baseline".into(),
        passed: water_ok,
        detail: format!(
            "surface_ul={}, vapor_ul={}, total_ul={}, initial_ul={}",
            surf_w.0, vapor_w.0, total_w.0, world.initial_world_water_ul
        ),
    });

    // Gate 8: L1 field validity (no NaN/Inf in electric potential, valid pH & temperatures)
    let mut bad_l1 = 0u64;
    for l1 in &world.l1_chunks {
        for i in 0..l1.ph_milli.len() {
            if l1.ph_milli[i] > 14_000
                || l1.temperature_dk[i] < 1000
                || !l1.electric_potential[i].is_finite()
            {
                bad_l1 += 1;
            }
        }
    }
    checks.push(ValidationCheck {
        name: "l1_fields_physical_bounds".into(),
        passed: bad_l1 == 0,
        detail: format!("invalid_l1_tiles={bad_l1}"),
    });

    let passed = checks.iter().all(|c| c.passed);
    WorldValidationReport { passed, checks }
}

/// Search exact AIR cells from the sinkhole mouths to the deep river.
fn verify_surface_to_blackwater_bfs(world: &WorldState) -> bool {
    let width = world.width_cells();
    let height = world.height_cells();
    let starts: Vec<_> = world
        .topology
        .sinkholes
        .iter()
        .map(|sh| CellCoord::new(sh.x_center, sh.top_y))
        .filter(|&cell| world.config.dimensions.contains_cell(cell))
        .collect();

    for &start in &starts {
        let x = start.x as usize;
        if underground_bfs(
            world,
            &[start],
            x.saturating_sub(40),
            (x + 41).min(width),
            height,
        ) {
            return true;
        }
    }
    !starts.is_empty() && underground_bfs(world, &starts, 0, width, height)
}

fn underground_bfs(
    world: &WorldState,
    starts: &[CellCoord],
    left: usize,
    right: usize,
    height: usize,
) -> bool {
    let span = right - left;
    let mut seen = vec![false; span * height];
    let mut queue = VecDeque::new();
    let mut visit = |x: usize, y: usize, queue: &mut VecDeque<(usize, usize)>| {
        if y > usize::from(world.topology.surface_height[x]) + 10 {
            return;
        }
        let index = y * span + x - left;
        if !seen[index] {
            seen[index] = true;
            if world.get_material(CellCoord::new(x as i32, y as i32)) == MaterialId::AIR {
                queue.push_back((x, y));
            }
        }
    };
    for cell in starts {
        visit(cell.x as usize, cell.y as usize, &mut queue);
    }
    while let Some((x, y)) = queue.pop_front() {
        let (kind, volume) = world.get_fluid(CellCoord::new(x as i32, y as i32));
        if kind == FluidKind::BlackWater && volume > 1000 {
            return true;
        }
        if y > 0 {
            visit(x, y - 1, &mut queue);
        }
        if x > left {
            visit(x - 1, y, &mut queue);
        }
        if x + 1 < right {
            visit(x + 1, y, &mut queue);
        }
        if y + 1 < height {
            visit(x, y + 1, &mut queue);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use plain_contracts::{MasterSeed, WorldConfig, WorldDimensions};
    use plain_world::SinkholeDescriptor;

    #[test]
    fn exact_bfs_reaches_the_aquifer_on_large_candidates() {
        for config in [
            WorldConfig::candidate_6144x2048(),
            WorldConfig::preset_8192x2048(),
        ] {
            let (world, _, _) =
                crate::generate_valid_world(config, MasterSeed::from_u64(404), 4).unwrap();
            let start = std::time::Instant::now();
            assert!(verify_surface_to_blackwater_bfs(&world));
            eprintln!(
                "{}x{} exact BFS: {:.3} ms",
                world.width_cells(),
                world.height_cells(),
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    #[test]
    fn one_cell_basalt_plate_seals_the_sinkhole() {
        let mut config = WorldConfig::preset_ci_1024x512();
        config.dimensions = WorldDimensions::from_chunks(1, 1);
        let mut world = WorldState::new_empty(config, MasterSeed::from_u64(1), 1);
        world.topology.surface_height = vec![50; 64];
        for y in 0..=50 {
            for x in 0..64 {
                if !(30..=32).contains(&x) {
                    world.set_solid_unchecked_gen(CellCoord::new(x, y), MaterialId::BASALT_BEDROCK);
                }
            }
        }
        world.topology.sinkholes.push(SinkholeDescriptor {
            region_id: plain_contracts::RegionId(0),
            x_center: 31,
            top_y: 56,
            bottom_y: 5,
            mean_radius: 1,
        });
        world.set_fluid_gen(CellCoord::new(31, 7), FluidKind::BlackWater, 9_000);
        let passes = |world: &WorldState| {
            validate_world(world)
                .checks
                .iter()
                .find(|check| check.name == "sinkhole_to_blackwater_connectivity")
                .unwrap()
                .passed
        };
        assert!(passes(&world));
        for x in 30..=32 {
            world.set_solid_unchecked_gen(CellCoord::new(x, 29), MaterialId::BASALT_BEDROCK);
        }
        assert!(!passes(&world));
    }
}
