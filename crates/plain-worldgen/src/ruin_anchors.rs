use plain_contracts::materials::MaterialId;
use plain_contracts::CellCoord;
use plain_world::spatial::CELL_FLAG_RUIN_ANCHOR;
use plain_world::{BasinRole, RuinAnchorKind, RuinGeologicalAnchor, WorldState};
use rand_core::RngCore;

use crate::underground::is_open_sinkhole_cell;

/// Step 5: Embed scattered, low-volume Ruin Geological Anchors (~10% ceiling, preserving ~90% natural world).
///
/// Stage 1 strictly reserves:
/// - 1 DeepRuinCore cavity with persistent waste-heat blocks (`RuinHeatCore`),
/// - Scattered surface/cliff/submerged outcrop foundations and cavities.
/// Zero Stage 4 mechanical, circuit, or ancient-AI logic is authored here.
pub fn embed_ruin_geological_anchors(world: &mut WorldState) {
    let w = world.width_cells() as i32;
    let h = world.height_cells() as i32;
    let mut rng = world.seed.derive_rng("ruin-anchors", 0);
    let mut anchors = Vec::new();
    let mut next_id = 1u32;

    // Scale ruin anchor count & modest shell dimensions with world area so scattered ruins
    // stay proportionally consistent ("common yet modest", well under ~10% natural-world budget)
    // across ANY configurable WorldDimensions (F3).
    let scale_x = ((w as f64) / 2048.0).clamp(0.75, 4.0);
    let scale_y = ((h as f64) / 1024.0).clamp(0.75, 2.5);

    // 1. DeepRuinCore: placed safely above the deep black-water aquifer near world center
    let core_x = (w / 2 + ((rng.next_u32() % 240) as i32 - 120)).clamp(120, w - 121);
    let aqy = world.topology.aquifer_y[core_x as usize] as i32;
    let sy = world.topology.surface_height[core_x as usize] as i32;
    let core_y = (aqy + 28).min(sy - 32).max(24);
    let core_hw = (28.0 * scale_x.min(2.2)) as i32;
    let core_hh = (14.0 * scale_y.min(2.0)) as i32;
    let core_wall_t = (3.0 * scale_y.min(2.0)) as i32;

    build_rectangular_ruin_shell(
        world,
        core_x - core_hw,
        core_y - core_hh,
        core_x + core_hw,
        core_y + core_hh,
        core_wall_t,
        true,
    );
    // Open side maintenance access breaches so the chamber is physically reachable from karst caves
    for dy in -4..=4 {
        for dx in 0..(core_wall_t + 3) {
            world.set_solid_unchecked_gen(
                CellCoord::new(core_x - core_hw - 1 + dx, core_y + dy),
                MaterialId::AIR,
            );
            world.set_solid_unchecked_gen(
                CellCoord::new(core_x + core_hw + 1 - dx, core_y + dy),
                MaterialId::AIR,
            );
        }
    }

    anchors.push(RuinGeologicalAnchor {
        id: next_id,
        kind: RuinAnchorKind::DeepRuinCore,
        region_id: world.region_for_x(core_x as usize),
        min_x: core_x - core_hw,
        min_y: core_y - core_hh,
        max_x: core_x + core_hw,
        max_y: core_y + core_hh,
        cavity_center: CellCoord::new(core_x, core_y),
        waste_heat_bonus_dk: 180, // +18.0 C deep waste heat emitter
    });
    next_id += 1;

    // 2. Scattered regional surface/cliff/submerged ruin outcrops (proportional count, modest footprint)
    let regions = world.regions.clone();
    for r in &regions {
        let span = (r.x_end - r.x_start) as i32;
        if span < 120 {
            continue;
        }
        let outcrops_in_basin = (span / 520).clamp(1, 4);
        for k in 0..outcrops_in_basin {
            let seg = span / outcrops_in_basin;
            let seg_start = r.x_start as i32 + k * seg;
            let ox = seg_start
                + seg / 4
                + ((rng.next_u32() as i32).unsigned_abs() % (seg / 2).max(1) as u32) as i32;
            let ox = ox.clamp(56, w - 57);
            let osy = world.topology.surface_height[ox as usize] as i32;

            let (kind, base_hw, base_hh, oy, heat) = match (r.role, k % 2) {
                (BasinRole::RidgePlateau, _) => (
                    RuinAnchorKind::PlateauTowerFoundation,
                    16,
                    10,
                    (osy + 2).min(h - 20),
                    0,
                ),
                (BasinRole::ElectrolyteSaltBasin, 0) => {
                    (RuinAnchorKind::BuriedTankPit, 18, 11, (osy - 7).max(28), 40)
                }
                (BasinRole::ElectrolyteSaltBasin, _) => (
                    RuinAnchorKind::SubmergedSluiceAnchor,
                    16,
                    10,
                    (world.topology.aquifer_y[ox as usize] as i32 + 6).max(24),
                    55,
                ),
                (BasinRole::CanyonTravertineTerrace, _) => (
                    RuinAnchorKind::CliffAirlockOutcrop,
                    15,
                    10,
                    (osy - 12).max(28),
                    20,
                ),
                (BasinRole::FaultBasin, _) => (
                    RuinAnchorKind::SubmergedSluiceAnchor,
                    16,
                    10,
                    (world.topology.aquifer_y[ox as usize] as i32 + 5).max(24),
                    60,
                ),
            };

            let hw = ((base_hw as f64) * scale_x.min(2.0)) as i32;
            let hh = ((base_hh as f64) * scale_y.min(1.8)) as i32;
            let wt = (2.0 * scale_y.min(1.8)).round() as i32;

            build_rectangular_ruin_shell(
                world,
                ox - hw,
                oy - hh,
                ox + hw,
                oy + hh,
                wt.max(2),
                false,
            );
            // Carve weathered top/side entrance breach
            for dx in -4..=4 {
                for dy in 0..(wt + 3) {
                    world.set_solid_unchecked_gen(
                        CellCoord::new(ox + dx, oy + hh - wt + dy),
                        MaterialId::AIR,
                    );
                }
            }

            anchors.push(RuinGeologicalAnchor {
                id: next_id,
                kind,
                region_id: r.id,
                min_x: ox - hw,
                min_y: oy - hh,
                max_x: ox + hw,
                max_y: oy + hh,
                cavity_center: CellCoord::new(ox, oy),
                waste_heat_bonus_dk: heat,
            });
            next_id += 1;
        }
    }

    // Count ruin solid cells vs total solid cells for validator reporting
    let mut ruin_cells = 0u64;
    let mut solid_cells = 0u64;
    for ch in &world.l0_chunks {
        for &m in &ch.solid_material {
            let mid = MaterialId(m);
            if mid.is_solid() {
                solid_cells += 1;
                if mid.is_ruin_material() {
                    ruin_cells += 1;
                }
            }
        }
    }

    world.topology.ruin_anchors = anchors;
    world.topology.initial_ruin_solid_cells = ruin_cells;
    world.topology.initial_total_solid_cells = solid_cells;
}

fn build_rectangular_ruin_shell(
    world: &mut WorldState,
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
    wall_t: i32,
    include_heat_core: bool,
) {
    let cx = (min_x + max_x) / 2;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let cell = CellCoord::new(x, y);
            if is_open_sinkhole_cell(world, cell) {
                continue;
            }
            let on_wall = x < min_x + wall_t
                || x > max_x - wall_t
                || y < min_y + wall_t
                || y > max_y - wall_t;
            if on_wall {
                let mat = if include_heat_core && y < min_y + wall_t + 1 && (x - cx).abs() <= 6 {
                    MaterialId::RUIN_HEAT_CORE
                } else if y == min_y + wall_t - 1 || y == max_y - wall_t + 1 {
                    MaterialId::RUIN_CONDUIT_CASING
                } else {
                    MaterialId::RUIN_ALLOY_WALL
                };
                world.set_solid_unchecked_gen(cell, mat);
            } else {
                world.set_solid_unchecked_gen(cell, MaterialId::AIR);
            }
            world.set_cell_flag(cell, CELL_FLAG_RUIN_ANCHOR);
        }
    }
}
