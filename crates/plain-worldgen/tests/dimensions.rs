use plain_contracts::{MasterSeed, WorldConfig, WorldDimensions};
use plain_world::WorldState;
use plain_worldgen::{generate_world, macro_layout::build_macro_layout, WorldGenError};

fn assert_basin_partition(world: &WorldState, count: usize) {
    assert_eq!(world.regions.len(), count);
    let mut end = 0;
    for region in &world.regions {
        assert_eq!(region.x_start, end);
        assert!(region.x_start < region.x_end && region.x_end <= world.width_cells());
        end = region.x_end;
    }
    assert_eq!(end, world.width_cells());
}

#[test]
fn minimum_world_dimensions_accept_the_full_basin_count_range() {
    for basin_count in [0, 1, 3, 4, 5, 6, 7, 8, 16] {
        let config = WorldConfig {
            dimensions: WorldDimensions::from_chunks(8, 6),
            basin_count,
            ..WorldConfig::default()
        };
        for seed in [2, 42, 777, 9001] {
            let (world, report) =
                generate_world(config.clone(), MasterSeed::from_u64(seed)).unwrap();
            assert_basin_partition(&world, basin_count.clamp(3, 8) as usize);
            assert_eq!(report.checks.len(), 8);
        }
    }
}

#[test]
fn standalone_layout_uses_available_width_and_generation_rejects_invalid_dimensions() {
    let seed = MasterSeed::from_u64(42);
    let mut config = WorldConfig {
        dimensions: WorldDimensions::from_chunks(1, 1),
        basin_count: 8,
        ..WorldConfig::default()
    };
    let mut world = WorldState::new_empty(config.clone(), seed, 1);
    build_macro_layout(&mut world);
    assert_basin_partition(&world, 8);

    for (width, height) in [
        (7, 6),
        (8, 5),
        (8, u16::MAX),
        (u16::MAX, 6),
        (u16::MAX, 1024),
    ] {
        config.dimensions = WorldDimensions::from_chunks(width, height);
        assert!(matches!(
            generate_world(config.clone(), seed),
            Err(WorldGenError::InvalidDimensions(x, y)) if (x, y) == (width, height)
        ));
    }
    config.dimensions = WorldDimensions::from_chunks(8, 6);
    config.meters_per_cell = f32::NAN;
    assert!(matches!(
        generate_world(config, seed),
        Err(WorldGenError::InvalidCellScale(_))
    ));
}
