use plain_contracts::materials::FluidKind;
use plain_contracts::{CellCoord, CELL_MAX_FLUID_UNITS};
use plain_world::{fluid, BasinRole, WorldState};

/// Step 6: Fill deep black-water aquifer, lowland salt marshes, stepped travertine pools,
/// canyon rivers & rain pools, then run equilibrium fluid settling and lock baseline water total.
pub fn initialize_hydrology(world: &mut WorldState) {
    let w = world.width_cells();
    let h = world.height_cells();

    // 1. Fill deep black-water aquifer cavities up to `aquifer_y`
    for x in 16..(w - 16) {
        let aqy = world.topology.aquifer_y[x] as usize;
        for y in 10..=aqy.min(h - 2) {
            let c = CellCoord::new(x as i32, y as i32);
            if world.get_material(c).is_air() {
                world.set_fluid_gen(c, FluidKind::BlackWater, CELL_MAX_FLUID_UNITS);
            }
        }
    }

    // 2. Fill lowland ElectrolyteSaltBasin surface depressions with SaltMarsh water
    let regions = world.regions.clone();
    for r in &regions {
        if r.role == BasinRole::ElectrolyteSaltBasin {
            // Find lowland water table ~25th percentile of basin surface height
            let mut heights: Vec<u16> = (r.x_start..r.x_end)
                .map(|x| world.topology.surface_height[x])
                .collect();
            heights.sort_unstable();
            if heights.is_empty() {
                continue;
            }
            let water_table = heights[(heights.len() * 35) / 100] as usize + 4;
            for x in (r.x_start + 8)..(r.x_end.saturating_sub(8)) {
                let sy = world.topology.surface_height[x] as usize;
                if sy < water_table {
                    for y in (sy + 1)..=water_table.min(h - 2) {
                        let c = CellCoord::new(x as i32, y as i32);
                        if world.get_material(c).is_air() {
                            world.set_fluid_gen(c, FluidKind::SaltMarsh, CELL_MAX_FLUID_UNITS);
                        }
                    }
                }
            }
        }
    }

    // 3. Fill Stepped Travertine Terrace pools behind calcified rims
    for &(tx0, tx1) in &world.topology.travertine_zones.clone() {
        for x in (tx0 + 3)..(tx1.saturating_sub(3)) {
            let sy = world.topology.surface_height[x] as usize;
            // Fill 4 cells above carved travertine bowl floor
            for dy in 1..=4 {
                let y = sy + dy;
                if y < h - 2 {
                    let c = CellCoord::new(x as i32, y as i32);
                    if world.get_material(c).is_air() {
                        world.set_fluid_gen(c, FluidKind::TravertinePool, CELL_MAX_FLUID_UNITS);
                    }
                }
            }
        }
    }

    // 4. Fill canyon wadis with shallow ThunderRiver water and scattered upland RainPools
    for &wx in &world.topology.flood_wadis.clone() {
        for dx in -10i32..=10i32 {
            let x = (wx + dx).clamp(8, (w as i32) - 9) as usize;
            let sy = world.topology.surface_height[x] as usize;
            for dy in 1..=3 {
                let c = CellCoord::new(x as i32, (sy + dy).min(h - 2) as i32);
                if world.get_material(c).is_air() {
                    world.set_fluid_gen(c, FluidKind::ThunderRiver, CELL_MAX_FLUID_UNITS);
                }
            }
        }
    }

    // 5. Run short static fluid equilibrium settling passes so initial pools sit stably
    let all_active = vec![true; world.l0_chunks.len()];
    for step in 0..12u64 {
        fluid::step_fluid_active_chunks(world, &all_active, step);
    }

    // 6. Seed L3 atmospheric water vapor reservoirs proportionally to basin width & moisture
    for r in &mut world.regions {
        let span = (r.x_end.saturating_sub(r.x_start)) as i64;
        // Atmospheric vapor bank scales with basin width and initial phase
        let base_vapor = span * 18_000;
        let phase_scale = (0.55 + (r.moisture_index as f64) * 0.70 + (r.dust_saturation as f64) * 0.35)
            .clamp(0.4, 1.6);
        r.water_vapor_ul = ((base_vapor as f64) * phase_scale) as i64;
        r.pending_surface_water_ul = 0;
    }

    // Record exact baseline conserved world water total (surface + pending + vapor)
    let (_, _, total_w) = world.total_world_water();
    world.initial_world_water_ul = total_w.0;
}
