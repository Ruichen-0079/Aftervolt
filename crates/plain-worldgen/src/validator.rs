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
    let min_sy = world.topology.surface_height.iter().copied().min().unwrap_or(0);
    let max_sy = world.topology.surface_height.iter().copied().max().unwrap_or(0);
    let relief_frac = ((max_sy.saturating_sub(min_sy)) as f32) / (h.max(1) as f32);
    checks.push(ValidationCheck {
        name: "surface_relief_contrast".into(),
        passed: relief_frac >= 0.18,
        detail: format!("min_y={min_sy}, max_y={max_sy}, relief={:.1}%", relief_frac * 100.0),
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

    // Gate 5: True coarse-grid BFS confirming open/water connectivity from surface down sinkhole to deep BlackWater
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
        for i in 0..l1.ph_scaled.len() {
            if l1.ph_scaled[i] > 140
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

/// Perform a 4-cell-strided BFS starting from the top of each carved sinkhole shaft to verify
/// an unobstructed non-solid shaft/cavern passage reaching actual deep `FluidKind::BlackWater`.
fn verify_surface_to_blackwater_bfs(world: &WorldState) -> bool {
    let stride = 4i32;
    let gw = (world.width_cells() as i32) / stride;
    let gh = (world.height_cells() as i32) / stride;
    let mut visited = vec![false; (gw * gh) as usize];
    let mut queue = VecDeque::new();

    for sh in &world.topology.sinkholes {
        let gx = (sh.x_center / stride).clamp(1, gw - 2);
        let gy = ((sh.top_y - 2) / stride).clamp(1, gh - 2);
        let idx = (gy * gw + gx) as usize;
        if !visited[idx] {
            visited[idx] = true;
            queue.push_back((gx, gy));
        }
    }

    let dirs = [(0, -1), (0, 1), (-1, 0), (1, 0)];
    while let Some((gx, gy)) = queue.pop_front() {
        let cx = gx * stride + stride / 2;
        let cy = gy * stride + stride / 2;
        let cell = CellCoord::new(cx, cy);
        let (fk, fv) = world.get_fluid(cell);
        if fk == FluidKind::BlackWater && fv > 1000 {
            return true;
        }
        for (dx, dy) in dirs {
            let nx = gx + dx;
            let ny = gy + dy;
            if nx <= 0 || ny <= 0 || nx >= gw - 1 || ny >= gh - 1 {
                continue;
            }
            let nidx = (ny * gw + nx) as usize;
            if visited[nidx] {
                continue;
            }
            // Passable if at least one cell in the 2x2 center of the strided node is Air
            let ncx = nx * stride + stride / 2;
            let ncy = ny * stride + stride / 2;
            let passable = world.get_material(CellCoord::new(ncx, ncy)) == MaterialId::AIR
                || world.get_material(CellCoord::new(ncx + 1, ncy)) == MaterialId::AIR
                || world.get_material(CellCoord::new(ncx, ncy + 1)) == MaterialId::AIR;
            if passable {
                visited[nidx] = true;
                queue.push_back((nx, ny));
            }
        }
    }
    false
}
