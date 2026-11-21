use std::ops::Range;

use plain_contracts::{
    CellCoord, MasterSeed, ViewportRect, WorldConfig, CELL_MAX_FLUID_UNITS, CHUNK_SIZE,
};
use plain_world::{
    fluid::step_fluid_active_chunks, spatial::MICROBE_SCALE, WorldSimulation, WorldState,
};
use plain_worldgen::generate_world;

#[derive(Clone, Copy, Debug)]
pub struct BasinSample {
    pub channel_level_m: f64,
    pub downstream_ul: i64,
    pub microbes: [f64; 4],
    pub conductivity: f64,
}

struct Survey {
    basin: Range<usize>,
    channel: Range<usize>,
    downstream: Range<usize>,
}

impl Survey {
    fn sample(&self, world: &WorldState) -> BasinSample {
        let surface_water = |x: usize| {
            let ground = usize::from(world.topology.surface_height[x]);
            (ground + 1..=(ground + 24).min(world.height_cells() - 1)).map(move |y| {
                (
                    y - ground,
                    world.get_fluid(CellCoord::new(x as i32, y as i32)).1,
                )
            })
        };
        let channel_level_m = self
            .channel
            .clone()
            .map(|x| {
                surface_water(x)
                    .filter(|&(_, volume)| volume > 0)
                    .map(|(above, volume)| {
                        (above - 1) as f64 + f64::from(volume) / f64::from(CELL_MAX_FLUID_UNITS)
                    })
                    .last()
                    .unwrap_or(0.0)
            })
            .sum::<f64>()
            * f64::from(world.config.meters_per_cell)
            / self.channel.len() as f64;
        let downstream_ul = self
            .downstream
            .clone()
            .flat_map(surface_water)
            .map(|(_, volume)| i64::from(volume))
            .sum();

        let ds = world.config.field_downsample.factor();
        let mut totals = [0; 5];
        let mut count = 0;
        for x in (ds / 2..world.width_cells())
            .step_by(ds)
            .filter(|x| self.basin.contains(x))
        {
            let ground = usize::from(world.surface_reference_height[x]);
            for y in (ds / 2..world.height_cells())
                .step_by(ds)
                .filter(|y| y.abs_diff(ground) <= 32)
            {
                let cell = CellCoord::new(x as i32, y as i32);
                let ci = world.chunk_index(cell.chunk_coord()).unwrap();
                let l1 = &world.l1_chunks[ci];
                let i = l1.idx(x % CHUNK_SIZE / ds, y % CHUNK_SIZE / ds);
                for (total, value) in totals.iter_mut().zip([
                    l1.microbe_saprotroph[i],
                    l1.microbe_conductive[i],
                    l1.microbe_halophile[i],
                    l1.microbe_ferro[i],
                    l1.conductivity[i],
                ]) {
                    *total += u64::from(value);
                }
                count += 1;
            }
        }
        assert!(count > 0 && downstream_ul > 0 && channel_level_m > 0.0);
        BasinSample {
            channel_level_m,
            downstream_ul,
            microbes: std::array::from_fn(|i| {
                totals[i] as f64 / (f64::from(MICROBE_SCALE) * f64::from(count))
            }),
            conductivity: totals[4] as f64 / f64::from(count),
        }
    }
}

pub fn run() -> [BasinSample; 3] {
    let mut config = WorldConfig::preset_ci_1024x512();
    // Accelerate only the game clock. The realtime arm still runs every fine
    // tick, with nonzero water exchange and multiple meso sweeps each day.
    config.active_lod0_padding_chunks = 0;
    config.clock.fine_hz = 8;
    config.clock.real_seconds_per_game_day = 4;
    config.clock.macro_steps_per_game_day = 4;
    config.clock.meso_period_ticks = 8;
    config.clock.electric_period_ticks = 8;
    let (mut initial, report) = generate_world(config, MasterSeed::from_u64(777)).unwrap();
    assert!(report.passed, "{}", report.failure_summary());

    // Bring the seeded pools to a common pre-run state before branching.
    let all = vec![true; initial.l0_chunks.len()];
    for tick in 0..96 {
        step_fluid_active_chunks(&mut initial, &all, tick);
    }
    let wadi = initial
        .topology
        .flood_wadis
        .iter()
        .copied()
        .map(|x| x as usize)
        .find(|&x| {
            let basin = &initial.regions[initial.region_for_x(x).0 as usize];
            x >= basin.x_start + 37 && x + 37 < basin.x_end
        })
        .expect("the fixed seed must include a wadi inside a basin");
    let elevation_sum = |span: Range<usize>| {
        span.map(|x| u64::from(initial.topology.surface_height[x]))
            .sum::<u64>()
    };
    let left = wadi - 36..wadi - 10;
    let right = wadi + 11..wadi + 37;
    let survey = Survey {
        basin: wadi - 36..wadi + 37,
        channel: wadi - 10..wadi + 11,
        downstream: if elevation_sum(left.clone()) < elevation_sum(right.clone()) {
            left
        } else {
            right
        },
    };
    let start = survey.sample(&initial);
    let heights = &initial.topology.surface_height[survey.basin.clone()];
    let viewport = ViewportRect {
        min_x: survey.basin.start as i32,
        max_x: (survey.basin.end - 1) as i32,
        min_y: i32::from(*heights.iter().min().unwrap()) - 36,
        max_y: i32::from(*heights.iter().max().unwrap()) + 36,
    };
    let starting_vapor = initial.total_world_water().1;
    let mut dormant = WorldSimulation::new(initial.clone());
    dormant.remove_simulation_anchor(0);
    let mut fast = WorldSimulation::new(initial.clone());
    fast.set_simulation_anchor(0, viewport);
    let mut live = WorldSimulation::new(initial);
    live.set_simulation_anchor(0, viewport);
    assert!((1..=16).contains(&live.active_lod0_count));

    const DAYS: u64 = 4;
    let spd = u64::from(live.world.clock.config.macro_steps_per_game_day);
    dormant.step_macro_steps(DAYS * spd, false);
    assert!(dormant
        .world
        .l0_chunks
        .iter()
        .all(|chunk| chunk.last_realized_macro_step == 0));
    dormant.set_simulation_anchor(0, viewport);
    dormant.step_macro_steps(0, true);
    for _ in 0..DAYS {
        fast.step_macro_steps(spd, true);
    }
    let ticks = live.world.clock.config.ticks_per_macro_step() * DAYS * spd;
    for _ in 0..ticks {
        live.step_fine_tick();
    }
    let endpoint = |sim: &WorldSimulation| sim.world.clock;
    assert_eq!(endpoint(&dormant), endpoint(&fast));
    assert_eq!(endpoint(&fast), endpoint(&live));
    for sim in [&dormant, &fast, &live] {
        assert_eq!(
            sim.world.total_world_water().2 .0,
            sim.world.initial_world_water_ul
        );
        assert_ne!(sim.world.total_world_water().1, starting_vapor);
        assert!(sim
            .world
            .l0_chunks
            .iter()
            .zip(sim.active_mask())
            .all(|(chunk, active)| !active || chunk.last_realized_macro_step == DAYS * spd));
        let meso = ticks / u64::from(sim.world.clock.config.meso_period_ticks);
        assert!(sim
            .world
            .l1_chunks
            .iter()
            .zip(sim.active_mask())
            .all(|(field, active)| !active || field.microbial_meso_step == meso));
    }
    let results = [
        survey.sample(&dormant.world),
        survey.sample(&fast.world),
        survey.sample(&live.world),
    ];
    for result in results {
        for (after, before) in result.microbes.into_iter().zip(start.microbes) {
            assert_ne!(after, before);
        }
        assert_ne!(result.conductivity, start.conductivity);
    }
    results
}

pub fn assert_close(samples: [BasinSample; 3]) {
    // The channel-level gate bounds the error to a fifth of one L0 cell. The
    // bank volume uses a relative budget because it spans several columns.
    for i in 0..samples.len() {
        for j in i + 1..samples.len() {
            let a = samples[i];
            let b = samples[j];
            assert!(
                (a.channel_level_m - b.channel_level_m).abs() <= 0.10,
                "water-level difference in modes {i}/{j}: {a:?} vs {b:?}"
            );
            let relative_pool = a.downstream_ul.abs_diff(b.downstream_ul) as f64
                / a.downstream_ul.max(b.downstream_ul) as f64;
            assert!(
                relative_pool <= 0.01,
                "downstream difference in modes {i}/{j}: {a:?} vs {b:?}"
            );
            for (guild, (left, right)) in a.microbes.into_iter().zip(b.microbes).enumerate() {
                assert!(
                    (left - right).abs() <= 0.5,
                    "guild {guild} difference in modes {i}/{j}: {left} vs {right}"
                );
            }
            assert!(
                (a.conductivity - b.conductivity).abs() <= 2.5,
                "conductivity difference in modes {i}/{j}: {a:?} vs {b:?}"
            );
        }
    }
}
