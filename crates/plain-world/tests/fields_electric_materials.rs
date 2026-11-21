use plain_contracts::materials::MaterialId;
use plain_contracts::{CellCoord, MasterSeed, WorldConfig, WorldDimensions, WorldPosMeters};
use plain_world::{electric, fields, WorldState};
use plain_worldgen::generate_world;

mod support;

use support::flat_test_world;

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
            world.l1_chunks[0].ph_milli[fi] = 3500;
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
                world.l1_chunks[0].ph_milli[fi],
                control.l1_chunks[0].ph_milli[fi]
            );
        }
    }
}

#[test]
fn l1_chunk_halos_propagate_and_respect_either_side_of_a_one_cell_wall() {
    for wall in [None, Some(63), Some(64)] {
        let mut world = flat_test_world();
        if let Some(x) = wall {
            for y in 0..64 {
                world.set_solid_unchecked_gen(CellCoord::new(x, y), MaterialId::BASALT_BEDROCK);
            }
        }
        let mut control = world.clone();
        world.l1_chunks[0].ph_milli.fill(3_500);
        world.l1_chunks[0].temperature_dk.fill(3_600);
        world.l1_chunks[0].fertility.fill(40_000);
        world.l1_chunks[0].salinity.fill(20_000);
        world.l1_chunks[0].electric_potential.fill(100.0);
        let mask = [true, true, false, false];
        for sim in [&mut world, &mut control] {
            electric::solve_local_electric_field(sim, &mask);
            fields::step_l1_fields(sim, &mask);
        }
        let a = &world.l1_chunks[1];
        let b = &control.l1_chunks[1];
        let i = a.idx(0, 5);
        let observed = [
            a.ph_milli[i] != b.ph_milli[i],
            a.temperature_dk[i] != b.temperature_dk[i],
            a.fertility[i] != b.fertility[i],
            a.salinity[i] != b.salinity[i],
            a.electric_potential[i] != b.electric_potential[i],
        ];
        assert_eq!(observed, [wall.is_none(); 5], "wall x={wall:?}");
    }
}

#[test]
#[cfg(feature = "dev-telemetry")]
fn potential_and_conductivity_overlays_measure_independent_fields() {
    use plain_contracts::{DevTelemetryPort, OverlayKind};
    use plain_world::WorldSimulation;

    let mut sim = WorldSimulation::new(flat_test_world());
    let image = |sim: &WorldSimulation, overlay| sim.render_world_overview(32, 32, overlay).rgba;
    let voltage = image(&sim, OverlayKind::ElectricPotential);
    let conductivity = image(&sim, OverlayKind::ConductivityField);
    sim.world.l1_chunks[0].conductivity.fill(1_500);
    let new_conductivity = image(&sim, OverlayKind::ConductivityField);
    assert_ne!(new_conductivity, conductivity);
    assert_eq!(image(&sim, OverlayKind::ElectricPotential), voltage);

    sim.world.l1_chunks[0].electric_potential.fill(140.0);
    assert_ne!(image(&sim, OverlayKind::ElectricPotential), voltage);
    assert_eq!(
        image(&sim, OverlayKind::ConductivityField),
        new_conductivity
    );
}

#[test]
fn ball_lightning_produces_sparse_moving_voltage_clusters_below_high_sky() {
    use plain_contracts::WeatherSpectacle;
    use std::collections::HashSet;

    let fixture = flat_test_world();
    let mut config = fixture.config;
    config.dimensions = WorldDimensions::from_chunks(12, 8);
    let mut charged = WorldState::new_empty(config, fixture.seed, fixture.worldgen_version);
    charged.topology.surface_height = vec![170; charged.width_cells()];
    for x in 0..charged.width_cells() as i32 {
        charged.set_solid_unchecked_gen(CellCoord::new(x, 170), MaterialId::BASALT_BEDROCK);
    }
    let mut region = fixture.regions[0].clone();
    region.x_end = charged.width_cells();
    region.weather = WeatherSpectacle::WanderingBallLightning;
    region.charge_index = 0.0;
    charged.regions.push(region);
    for field in &mut charged.l1_chunks {
        field.conductivity.fill(24);
    }
    let mut control = charged.clone();
    control.regions[0].weather = WeatherSpectacle::Calm;
    // Match the uniform overhead boundary so the comparison isolates ball sources.
    control.regions[0].charge_index = (95.0 - 10.0) / 120.0;
    let active = vec![true; charged.l0_chunks.len()];
    let ds = charged.config.field_downsample.factor() as i32;

    let solve = |mut storm: WorldState, tick| {
        storm.clock.tick.0 = tick;
        for _ in 0..10 {
            electric::solve_local_electric_field(&mut storm, &active);
        }
        storm
    };
    let calm = solve(control, 0);
    let inspect = |storm: &WorldState| {
        let mut highlights = HashSet::new();
        let sky = (storm.height_cells() as i32 - 1) / ds;
        let width = storm.width_cells() as i32 / ds;
        for fy in 0..=sky {
            for fx in 0..width {
                let cell = CellCoord::new(fx * ds + ds / 2, fy * ds + ds / 2);
                let voltage = storm.sample_l1_at_cell(cell).unwrap().electric_potential;
                let background = calm.sample_l1_at_cell(cell).unwrap().electric_potential;
                if voltage - background > 30.0 {
                    assert!((180..=310).contains(&cell.y), "bright tile at {cell:?}");
                    highlights.insert((fx, fy));
                }
            }
        }
        assert!(highlights.len() < 150);
        let mut unvisited = highlights.clone();
        let mut groups = 0;
        while let Some(&start) = unvisited.iter().next() {
            groups += 1;
            unvisited.remove(&start);
            let mut queue = vec![start];
            while let Some((x, y)) = queue.pop() {
                for adjacent in [(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)] {
                    if unvisited.remove(&adjacent) {
                        queue.push(adjacent);
                    }
                }
            }
        }
        assert!(
            (2..=4).contains(&groups),
            "observed {groups} voltage clusters"
        );
        highlights
    };

    let first = inspect(&solve(charged.clone(), 0));
    assert_eq!(first, inspect(&solve(charged.clone(), 0)));
    assert_ne!(first, inspect(&solve(charged, 617)));
}
