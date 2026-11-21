use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::{CellCoord, MasterSeed, WorldConfig};
use plain_world::{catchup::catch_up_chunk, fields, WorldSimulation};
use plain_worldgen::generate_world;

mod support;

use support::flat_test_world;

#[test]
fn conductive_microbes_retain_each_half_unit_in_realtime_and_replay() {
    use plain_world::MacroFluxRecord;

    let mut live = flat_test_world();
    live.regions[0].moisture_index = 0.9;
    live.clock.config.meso_period_ticks = 2;
    live.clock.config.real_seconds_per_game_day = 4;
    live.clock.config.fine_hz = 1;
    live.clock.config.macro_steps_per_game_day = 1;
    live.config.clock = live.clock.config;
    let cell = CellCoord::new(10, 10);
    live.set_solid_unchecked_gen(cell, MaterialId::BASALT_BEDROCK);
    for fields in &mut live.l1_chunks {
        fields.ph_milli.fill(6_250);
    }
    let mut asleep = live.clone();
    let fields = &live.l1_chunks[0];
    let i = fields.idx(2, 2);
    let initial = fields.microbe_conductive[i];
    let displayed = live.sample_l1_at_cell(cell).unwrap().microbes[1];
    let initial_conductivity = fields.conductivity[i];
    for elapsed in 1..=2 {
        fields::step_l1_fields(&mut live, &[true; 4]);
        assert_eq!(
            live.l1_chunks[0].microbe_conductive[i],
            initial + 50 * elapsed
        );
    }
    assert_eq!(
        live.sample_l1_at_cell(cell).unwrap().microbes[1],
        displayed + 1
    );

    asleep.clock.advance_macro_steps(1);
    asleep.regions[0].flux_history.push(MacroFluxRecord {
        macro_step: 1,
        era: plain_contracts::EraPhase::FlowerSeaBloom as u8,
        moisture: Some(0.9),
        ..Default::default()
    });
    assert!(catch_up_chunk(&mut asleep, 0));
    let field = &asleep.l1_chunks[0];
    assert_eq!(field.microbe_conductive[i], initial + 100);
    assert_eq!(field.microbial_meso_step, 2);
    assert_ne!(field.conductivity[i], initial_conductivity);
    assert!(field.conductivity[i].abs_diff(live.l1_chunks[0].conductivity[i]) <= 1);
}

#[test]
fn groundwater_transpires_only_through_connected_air_or_permeable_rock() {
    use plain_contracts::EraPhase;

    let mut world = flat_test_world();
    world.config.clock.macro_steps_per_game_day = 1;
    world.clock.config = world.config.clock;
    world.regions[0].era = EraPhase::SilentReverseRain;
    world.regions[0].charge_index = 0.9;
    world.regions[0].dust_saturation = 0.0;
    for x in 0..world.width_cells() as i32 {
        for y in 0..=63 {
            world.set_solid_unchecked_gen(CellCoord::new(x, y), MaterialId::BASALT_BEDROCK);
        }
    }
    let isolated = CellCoord::new(20, 20);
    let river = CellCoord::new(80, 20);
    let gate = CellCoord::new(80, 36);
    world.set_solid_unchecked_gen(isolated, MaterialId::AIR);
    for y in 20..=63 {
        world.set_solid_unchecked_gen(CellCoord::new(river.x, y), MaterialId::AIR);
    }
    for cell in [isolated, river] {
        world.set_fluid_gen(cell, FluidKind::BlackWater, 9_000);
    }

    let mut sim = WorldSimulation::new(world);
    let initial_water = sim.world.total_world_water().2;
    let mut evaporated = 0;
    for (gate_material, expected) in [
        (MaterialId::BASALT_BEDROCK, 0),
        (MaterialId::SANDSTONE, 600),
        (MaterialId::RUIN_ALLOY_WALL, 0),
        (MaterialId::AIR, 600),
    ] {
        sim.world.set_solid_unchecked_gen(gate, gate_material);
        sim.world.regions[0].evaporation_remainder_ul = 600;
        sim.step_macro_steps(1, false);
        evaporated += expected;
        assert_eq!(sim.world.get_fluid(isolated).1, 9_000);
        assert_eq!(sim.world.get_fluid(river).1, 9_000 - evaporated);
        assert_eq!(sim.world.regions[0].water_vapor_ul, i64::from(evaporated));
        assert_eq!(sim.world.total_world_water().2, initial_water);
    }
}

#[test]
fn erosion_below_reference_follows_material_law_across_vertical_seam() {
    use plain_contracts::EraPhase;
    use plain_world::MacroFluxRecord;

    let mut world = flat_test_world();
    world.config.clock.macro_steps_per_game_day = 1;
    world.clock.config = world.config.clock;
    world.topology.surface_height.fill(64);
    world.surface_reference_height = vec![74; world.width_cells()];
    for x in 0..world.width_cells() as i32 {
        let heights = if x == 2 { 8..=8 } else { 59..=64 };
        for y in heights {
            let mat = if x == 0 || x == 2 {
                MaterialId::LOOSE_SOIL_DAM
            } else {
                MaterialId::BASALT_BEDROCK
            };
            world.set_solid_unchecked_gen(CellCoord::new(x, y), mat);
        }
    }

    world.topology.surface_height[2] = 8;

    world.clock.advance_macro_steps(14);
    for step in 1..=14 {
        world.regions[0].flux_history.push(MacroFluxRecord {
            macro_step: step,
            era: EraPhase::RustRedRainFlood as u8,
            acid_erosion_impulse: 2.0,
            flood_peak: 1.0,
            net_water_delta_per_col: if step == 14 { 100 } else { 0 },
            ..Default::default()
        });
    }
    world.regions[0].pending_surface_water_ul = 100 * world.width_cells() as i64;
    let initial_water = world.total_world_water().2;
    let mut sim = WorldSimulation::new(world);
    sim.catch_up_all_chunks();
    let world = &sim.world;
    let eroded = world.topology.surface_height[0];
    assert!(eroded <= 62, "soft surface elevation {eroded}");
    assert_eq!(world.topology.surface_height[1], 64);
    assert_eq!(world.topology.surface_height[2], 8);
    assert_eq!(
        world.get_material(CellCoord::new(1, 64)),
        MaterialId::BASALT_BEDROCK
    );
    assert_eq!(
        world.get_fluid(CellCoord::new(0, i32::from(eroded) + 1)).1,
        100
    );
    assert_eq!(world.get_fluid(CellCoord::new(1, 65)).1, 100);
    assert_eq!(world.total_world_water().2, initial_water);
}

#[test]
fn natural_sinkhole_connects_the_dark_river_to_the_evaporation_ledger() {
    use plain_contracts::EraPhase;

    let (mut world, report) =
        generate_world(WorldConfig::preset_ci_1024x512(), MasterSeed::from_u64(42)).unwrap();
    assert!(report.passed, "{}", report.failure_summary());
    assert!(!world.topology.sinkholes.is_empty());
    let initial_water = world.initial_world_water_ul;
    world.config.clock.macro_steps_per_game_day = 1;
    world.clock.config = world.config.clock;
    world
        .config
        .four_era_cycle
        .cross_basin_wind_transfer_per_day = 0.0;

    // Remove shallow water to the owning atmosphere, leaving the natural L0
    // geology and every deep pool exactly as generated.
    for x in 0..world.width_cells() {
        let ground = usize::from(world.topology.surface_height[x]);
        for y in ground.saturating_sub(24)..world.height_cells() {
            let cell = CellCoord::new(x as i32, y as i32);
            let (kind, volume) = world.get_fluid(cell);
            if volume != 0 {
                let basin = world.region_for_x(x).0 as usize;
                world.regions[basin].water_vapor_ul += i64::from(volume);
                world.set_fluid_gen(cell, kind, 0);
            }
        }
    }
    for basin in &mut world.regions {
        basin.era = EraPhase::SilentReverseRain;
        basin.charge_index = 0.9;
        basin.dust_saturation = 0.0;
    }
    let basin = world
        .region_for_x(world.topology.sinkholes[0].x_center as usize)
        .0 as usize;
    let vapor = world.regions[basin].water_vapor_ul;
    world.regions[basin].evaporation_remainder_ul = 1_000;
    let mut sim = WorldSimulation::new(world);
    sim.step_macro_steps(1, false);
    assert_eq!(sim.world.regions[basin].water_vapor_ul - vapor, 1_000);
    assert_eq!(sim.world.total_world_water().2 .0, initial_water);
}

#[test]
fn dormant_microbes_follow_archived_wet_and_dry_intervals() {
    use plain_contracts::EraPhase;
    use plain_world::{MacroFluxRecord, RegionalMacroFluxHistory};

    let mut world = flat_test_world();
    world.config.clock.fine_hz = 1;
    world.config.clock.real_seconds_per_game_day = 1;
    world.config.clock.macro_steps_per_game_day = 1;
    world.config.clock.meso_period_ticks = 1;
    world.clock.config = world.config.clock;
    world.l1_chunks[0].ph_milli.fill(6_250);
    world.set_solid_unchecked_gen(CellCoord::new(1, 1), MaterialId::BASALT_BEDROCK);
    let basin = &mut world.regions[0];
    basin.moisture_index = 0.1;
    basin.flux_history = RegionalMacroFluxHistory::new(2);
    for step in 1..=6 {
        basin.flux_history.push(MacroFluxRecord {
            macro_step: step,
            era: EraPhase::RustRedRainFlood as u8,
            moisture: Some(if step <= 3 { 0.9 } else { 0.1 }),
            ..Default::default()
        });
    }
    assert!(basin.flux_history.discarded_through >= 3);
    let expected = (0..3).fold(1_650u32, |density, _| (density * 985 + 500) / 1000);
    world.clock.advance_macro_steps(6);
    assert!(catch_up_chunk(&mut world, 0));
    assert_eq!(
        u32::from(world.l1_chunks[0].microbe_conductive[0]),
        expected
    );
}
