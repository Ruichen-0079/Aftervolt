use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::CellCoord;

mod support;

use support::flat_test_world;

#[test]
fn diagonal_drainage_crosses_side_vertical_and_corner_seams() {
    use plain_world::fluid::step_fluid_active_chunks;

    for direction in [1, -1] {
        let base_x = if direction > 0 { 62 } else { 65 };
        for (shift_x, shift_y) in [(0, 0), (direction, 0), (0, 1), (direction, 1)] {
            let source = CellCoord::new(base_x + shift_x, 63 + shift_y);
            let outlet = CellCoord::new(source.x + direction, source.y - 3);
            let mut world = flat_test_world();
            for dx in -2..=2 {
                for dy in -4..=1 {
                    if (dx, dy) != (0, 0) && !(dx == direction && (-3..=-1).contains(&dy)) {
                        world.set_solid_unchecked_gen(
                            CellCoord::new(source.x + dx, source.y + dy),
                            MaterialId::BASALT_BEDROCK,
                        );
                    }
                }
            }
            world.set_fluid_gen(source, FluidKind::RainPool, 10_000);
            let initial = world.total_world_water().2;
            let neighbor = CellCoord::new(source.x + direction, source.y - 1);
            if source.chunk_coord() != neighbor.chunk_coord() {
                let mut inactive = [false; 4];
                inactive[world.chunk_index(source.chunk_coord()).unwrap()] = true;
                step_fluid_active_chunks(&mut world, &inactive, 0);
                assert_eq!(world.get_fluid(source).1, 10_000);
                assert_eq!(world.get_fluid(neighbor).1, 0);
            }
            for tick in 0..40 {
                step_fluid_active_chunks(&mut world, &[true; 4], tick);
                assert_eq!(world.total_world_water().2, initial);
            }
            assert!(world.get_fluid(outlet).1 >= 9_984, "outlet {outlet:?}");
            assert!(world.get_fluid(source).1 <= 16, "source {source:?}");
        }
    }
}

#[test]
fn lateral_leveling_has_the_same_minimum_volume_and_head_on_a_seam() {
    use plain_world::fluid::step_fluid_active_chunks;

    for x in [61, 63] {
        for (start, next, expected) in [(12, 0, 0), (20, 10, 10), (13, 0, 4)] {
            let mut world = flat_test_world();
            for dx in -1..=2 {
                world.set_solid_unchecked_gen(
                    CellCoord::new(x + dx, 23),
                    MaterialId::BASALT_BEDROCK,
                );
            }
            for dx in [-1, 2] {
                world.set_solid_unchecked_gen(
                    CellCoord::new(x + dx, 24),
                    MaterialId::BASALT_BEDROCK,
                );
            }
            let source = CellCoord::new(x, 24);
            let target = CellCoord::new(x + 1, 24);
            world.set_fluid_gen(source, FluidKind::RainPool, start);
            world.set_fluid_gen(target, FluidKind::RainPool, next);
            step_fluid_active_chunks(&mut world, &[true; 4], 0);
            assert_eq!(world.get_fluid(target).1, expected);
            assert_eq!(world.get_fluid(source).1, start + next - expected);
        }
    }
}

#[test]
fn seam_diagonal_ties_alternate_with_tick_parity() {
    use plain_world::fluid::step_fluid_active_chunks;

    for parity in [0, 1] {
        let mut world = flat_test_world();
        let source = CellCoord::new(63, 64);
        for (x, y) in [(63, 63), (62, 64), (64, 64)] {
            world.set_solid_unchecked_gen(CellCoord::new(x, y), MaterialId::BASALT_BEDROCK);
        }
        world.set_fluid_gen(source, FluidKind::RainPool, 10_000);
        step_fluid_active_chunks(&mut world, &[true; 4], parity);
        assert_eq!(world.get_fluid(source).1, 2_500);
        let left = world.get_fluid(CellCoord::new(62, 63)).1;
        let right = world.get_fluid(CellCoord::new(64, 63)).1;
        assert_eq!(
            (left, right),
            if parity == 0 {
                (5_000, 2_500)
            } else {
                (2_500, 5_000)
            }
        );
        assert_eq!(world.total_world_water().2 .0, 10_000);
    }
}
