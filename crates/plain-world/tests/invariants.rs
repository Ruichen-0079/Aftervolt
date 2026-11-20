use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::telemetry::ViewportRect;
use plain_contracts::{CellCoord, MasterSeed, WorldConfig, WorldDimensions, WorldPosMeters};
use plain_world::{catchup::catch_up_chunk, electric, fields, WorldSimulation, WorldState};
use plain_worldgen::generate_world;

/// F5 Invariant: Terrain modification strictly obeys MaterialDef physical properties —
/// NO hardcoded indestructible blocks, yet high yield-strength materials naturally resist weak forces.
#[test]
fn material_law_wear_has_no_indestructible_special_cases() {
    let cfg = WorldConfig::preset_ci_1024x512();
    let (mut world, report) = generate_world(cfg, MasterSeed::from_u64(11)).unwrap();
    assert!(report.passed);

    let test_salt = CellCoord::new(200, 200);
    let test_alloy = CellCoord::new(202, 200);
    world.set_solid_unchecked_gen(test_salt, MaterialId::SALT_CRUST);
    world.set_solid_unchecked_gen(test_alloy, MaterialId::RUIN_ALLOY_WALL);

    // Moderate acid dissolves SaltCrust quickly, leaves RuinAlloyWall intact
    let mut salt_dissolved = false;
    for _ in 0..30 {
        if world.apply_physical_wear(test_salt, 0, 2.0, 0.0) {
            salt_dissolved = true;
            break;
        }
    }
    assert!(
        salt_dissolved,
        "SaltCrust should dissolve under sustained acid rain"
    );
    assert_eq!(world.get_material(test_salt), MaterialId::AIR);

    // Weak hand-level stress (5,000 kPa) does zero mechanical damage to RuinAlloyWall (yield 360,000 kPa)
    assert!(!world.apply_physical_wear(test_alloy, 5_000, 0.0, 0.0));
    assert_eq!(world.get_material(test_alloy), MaterialId::RUIN_ALLOY_WALL);

    // Extreme industrial demolition stress (> yield strength) CAN physically breach RuinAlloyWall
    let mut alloy_breached = false;
    for _ in 0..40 {
        if world.apply_physical_wear(test_alloy, 500_000, 0.0, 5.0) {
            alloy_breached = true;
            break;
        }
    }
    assert!(
        alloy_breached,
        "Even RuinAlloyWall must physically yield under extreme industrial stress (F5)"
    );
    assert_ne!(world.get_material(test_alloy), MaterialId::RUIN_ALLOY_WALL);
}

fn flat_test_world() -> WorldState {
    let mut cfg = WorldConfig::preset_ci_1024x512();
    cfg.dimensions = WorldDimensions::from_chunks(2, 2);
    cfg.active_lod0_padding_chunks = 0;
    cfg.four_era_cycle.cross_basin_wind_transfer_per_day = 0.0;
    let mut world = WorldState::new_empty(cfg, MasterSeed::from_u64(12), 1);
    world.topology.surface_height = vec![63; world.width_cells()];
    world.regions.push(plain_world::MacroRegion {
        id: plain_contracts::RegionId(0),
        name: "Test".into(),
        role: plain_world::BasinRole::ElectrolyteSaltBasin,
        x_start: 0,
        x_end: world.width_cells(),
        mean_surface_y: 256,
        water_vapor_ul: 0,
        pending_surface_water_ul: 0,
        precipitation_remainder_ul: 0,
        evaporation_remainder_ul: 0,
        mineral_dust_units: 0,
        surface_charge_uc: 0,
        moisture_index: 0.5,
        fertility_index: 0.5,
        salt_crust_ratio: 0.2,
        charge_index: 0.2,
        dust_saturation: 0.2,
        flower_sea_suitability: 0.5,
        era: plain_contracts::EraPhase::FlowerSeaBloom,
        weather: plain_contracts::WeatherSpectacle::Calm,
        era_elapsed_macro_steps: 0,
        era_transitions_count: 0,
        flux_history: plain_world::RegionalMacroFluxHistory::new(256),
    });

    world
}

#[test]
fn l1_wall_aware_flux_mask_blocks_thin_wall_leakage() {
    let mut world = flat_test_world();
    for y in 0..64 {
        world.set_solid_unchecked_gen(CellCoord::new(7, y), MaterialId::BASALT_BEDROCK);
    }
    world.recompute_chunk_flux_mask(0);

    let axis = world.config.field_downsample.tiles_per_chunk_axis();
    let left_adj = world.l1_chunks[0].idx(1, 5);
    let right_adj = world.l1_chunks[0].idx(2, 5);
    assert_ne!(
        world.l1_chunks[0].flux_block_mask[left_adj] & plain_world::FLUX_BLOCK_E,
        0
    );
    assert_eq!(
        world.l1_chunks[0].flux_block_mask[right_adj] & plain_world::FLUX_BLOCK_W,
        0
    );

    let mut control = world.clone();
    for fy in 0..axis {
        for fx in 0..2 {
            let fi = world.l1_chunks[0].idx(fx, fy);
            world.l1_chunks[0].ph_scaled[fi] = 35;
            world.l1_chunks[0].electric_potential[fi] = 100.0;
        }
    }

    let pos = WorldPosMeters::new(9.0 * 0.5, 21.0 * 0.5);
    assert_eq!(
        electric::sample_electric_gradient(&world, pos, false).grad_x,
        0.0
    );

    let mask = [true, false, false, false];
    electric::solve_local_electric_field(&mut world, &mask);
    assert!(world.l1_chunks[0].electric_potential[left_adj] > 0.0);
    for _ in 0..20 {
        fields::step_l1_fields(&mut world, &mask);
        fields::step_l1_fields(&mut control, &mask);
    }

    for fy in 0..axis {
        for fx in 2..axis {
            let fi = world.l1_chunks[0].idx(fx, fy);
            assert_eq!(world.l1_chunks[0].electric_potential[fi], 0.0);
            assert_eq!(
                world.l1_chunks[0].ph_scaled[fi],
                control.l1_chunks[0].ph_scaled[fi]
            );
        }
    }
}

#[test]
fn fine_tick_realizes_the_macro_record_at_the_boundary() {
    let mut world = flat_test_world();
    world.clock.config.electric_period_ticks = u32::MAX;
    world.clock.config.meso_period_ticks = u32::MAX;
    let boundary = world.clock.config.ticks_per_macro_step();
    world.clock.tick.0 = boundary - 1;
    let mut direct = WorldSimulation::new(world.clone());
    direct.world.clock.tick.0 = 0;
    direct.step_macro_steps(1, false);
    assert!(catch_up_chunk(&mut direct.world, 0));

    let mut fine = WorldSimulation::new(world);
    fine.set_dev_camera(
        0,
        ViewportRect {
            min_x: 0,
            min_y: 0,
            max_x: 0,
            max_y: 0,
        },
    );
    let initial_fertility = fine.world.l1_chunks[0].fertility[0];
    fine.step_fine_tick();
    assert_eq!(fine.world.clock.tick.0, boundary);
    assert_eq!(fine.world.l0_chunks[0].last_realized_macro_step, 1);
    assert_ne!(fine.world.l1_chunks[0].fertility[0], initial_fertility);
    assert_eq!(
        fine.world.l1_chunks[0].fertility,
        direct.world.l1_chunks[0].fertility
    );
    assert_eq!(
        fine.world.l1_chunks[0].salinity,
        direct.world.l1_chunks[0].salinity
    );
    assert_eq!(
        fine.world.l1_chunks[0].ph_scaled,
        direct.world.l1_chunks[0].ph_scaled
    );
}

#[test]
fn catchup_advances_only_to_published_records() {
    let mut world = flat_test_world();
    world.clock.macro_step.0 = 1;
    assert!(!catch_up_chunk(&mut world, 0));
    assert_eq!(world.l0_chunks[0].last_realized_macro_step, 0);

    plain_world::climate::step_macro_climate(&mut world);
    world.clock.macro_step.0 = 2;
    assert!(catch_up_chunk(&mut world, 0));
    assert_eq!(world.l0_chunks[0].last_realized_macro_step, 1);
    assert!(!catch_up_chunk(&mut world, 0));

    world.clock.macro_step.0 = 3;
    plain_world::climate::step_macro_climate(&mut world);
    assert!(!catch_up_chunk(&mut world, 0));
    assert_eq!(world.l0_chunks[0].last_realized_macro_step, 1);
}

#[test]
fn small_evaporation_steps_accumulate_and_credit_only_real_surface_water() {
    let mut world = flat_test_world();
    world.config.clock.macro_steps_per_game_day = 120;
    world.clock.config = world.config.clock;
    world.regions[0].pending_surface_water_ul = 1;
    world.set_solid_unchecked_gen(CellCoord::new(0, 63), MaterialId::BASALT_BEDROCK);
    let bottom = CellCoord::new(0, 64);
    let surface = CellCoord::new(0, 65);
    world.set_fluid_gen(bottom, FluidKind::SaltMarsh, 100);
    world.set_fluid_gen(surface, FluidKind::SaltMarsh, 100);
    let initial_total = world.total_world_water().2;

    let mut sim = WorldSimulation::new(world);
    sim.step_macro_steps(1, false);
    let r = &sim.world.regions[0];
    assert_eq!(r.pending_surface_water_ul, 0);
    assert_eq!(r.water_vapor_ul, 1);
    assert!(r.evaporation_remainder_ul > 0);
    assert_eq!(r.flux_history.records[0].net_water_delta_per_col, 0);
    sim.step_macro_steps(119, false);
    assert_eq!(sim.world.regions[0].water_vapor_ul, 1);
    let emitted = -sim.world.regions[0]
        .flux_history
        .records
        .iter()
        .map(|r| r.net_water_delta_per_col)
        .sum::<i32>();
    assert!((1..100).contains(&emitted));
    assert!(catch_up_chunk(&mut sim.world, 0));
    assert!(catch_up_chunk(&mut sim.world, 2));
    assert_eq!(sim.world.get_fluid(bottom).1, 100);
    assert_eq!(sim.world.get_fluid(surface).1, 100 - emitted as u16);
    assert_eq!(sim.world.regions[0].water_vapor_ul, 1 + i64::from(emitted));
    assert_eq!(sim.world.regions[0].pending_surface_water_ul, 0);
    assert_eq!(sim.world.total_world_water().2, initial_total);
}

/// F6 & E.4 Invariant: Exact integer water conservation & self-sustaining 4-era oscillation
/// across 3 x 1220 = 3660 game days + full catch-up.
#[test]
fn three_cycles_3660_days_exact_water_conservation_and_oscillation() {
    let cfg = WorldConfig::preset_ci_1024x512();
    let (world, report) = generate_world(cfg, MasterSeed::from_u64(42)).unwrap();
    assert!(
        report.passed,
        "Validation failed: {}",
        report.failure_summary()
    );

    let mut sim = WorldSimulation::new(world);
    let init_water = sim.world.initial_world_water_ul;

    // Step 60 fine ticks first
    for _ in 0..60 {
        sim.step_fine_tick();
    }
    let (_, _, w_after_fine) = sim.world.total_world_water();
    assert_eq!(w_after_fine.0, init_water, "Fine fluid step leaked water!");

    // Step 3 x 1220 = 3660 game days in macro mode
    let steps = 3660u64 * (sim.world.config.clock.macro_steps_per_game_day as u64);
    let mut transitions = 0;
    for _ in 0..steps {
        transitions += sim.step_macro_steps(1, false);
        assert!(sim.world.regions.iter().all(|r| {
            r.pending_surface_water_ul >= 0
                && r.water_vapor_ul >= 0
                && (0..=r.pending_surface_water_ul).contains(&r.precipitation_remainder_ul)
        }));
    }
    sim.catch_up_all_chunks();
    assert!(sim
        .world
        .regions
        .iter()
        .all(|r| { r.pending_surface_water_ul >= 0 && r.water_vapor_ul >= 0 }));

    let (_, _, w_after_3cycles) = sim.world.total_world_water();
    assert_eq!(
        w_after_3cycles.0, init_water,
        "3x1220-day macro + catch-up leaked water!"
    );
    assert!(
        transitions >= (sim.world.regions.len() as u32) * 8,
        "Four-era cycle must self-sustain across 3660 days (got {transitions} transitions)"
    );
}

#[test]
fn rain_remainders_are_realized_only_in_the_owning_basin() {
    let mut world = flat_test_world();
    let mut east = world.regions[0].clone();
    east.id.0 = 1;
    east.x_start = 15;
    world.regions[0].x_end = 15;
    world.regions[0].era = plain_contracts::EraPhase::RustRedRainFlood;
    world.regions[0].water_vapor_ul = 20_000;
    world.regions.push(east);
    for x in 0..64 {
        world.set_solid_unchecked_gen(CellCoord::new(x, 63), MaterialId::BASALT_BEDROCK);
    }
    let initial_total = world.total_world_water().2;
    let mut sim = WorldSimulation::new(world);
    sim.step_macro_steps(1, false);
    assert_eq!(
        sim.world.regions[0].flux_history.records[0].net_water_delta_per_col,
        0
    );
    assert!(sim.world.regions[0].precipitation_remainder_ul > 0);
    sim.step_macro_steps(59, false);
    let rain = &sim.world.regions[0];
    let expected = rain
        .flux_history
        .records
        .iter()
        .map(|r| r.net_water_delta_per_col)
        .sum::<i32>();
    assert!(expected > 0);
    assert_eq!(
        rain.pending_surface_water_ul - rain.precipitation_remainder_ul,
        i64::from(expected) * 15
    );
    assert!(catch_up_chunk(&mut sim.world, 0));
    assert!(catch_up_chunk(&mut sim.world, 2));
    for x in 0..64 {
        let actual = sim.world.get_fluid(CellCoord::new(x, 64)).1;
        assert_eq!(actual, if x < 15 { expected as u16 } else { 0 });
    }
    let rain = &sim.world.regions[0];
    assert_eq!(
        rain.pending_surface_water_ul,
        rain.precipitation_remainder_ul
    );
    assert_eq!(sim.world.total_world_water().2, initial_total);
}
