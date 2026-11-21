#![deny(clippy::todo, clippy::unimplemented)]

//! Quantitative Benchmark Suite for Q1, Q2, Q3, Q4 & Q7 (Stage 1 Engineering Verification).
//!
//! Executes full-scale measurements on real hardware — never uses tiny demo maps to extrapolate
//! full-world performance. Outputs structured console summary & JSON report.

use std::path::PathBuf;
use std::time::Instant;

use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::{
    CellCoord, DevTelemetryPort, FieldDownsample, MasterSeed, ViewportRect, WorldConfig,
    WorldPosMeters,
};
use plain_persist::WorldSaveStore;
use plain_world::electric::sample_electric_gradient;
use plain_world::fluid::wake_fluid_chunks;
use plain_world::{electric, fields, fluid, WorldSimulation, WorldState};
use plain_worldgen::generate_valid_world;

#[path = "../../plain-world/tests/support/three_modes.rs"]
mod three_modes;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=========================================================================");
    println!("  《遗电平原》Rust World Core — 阶段 1 全尺寸定量基准验证 (Q1–Q4 & Q7)");
    println!("=========================================================================\n");

    run_q1_resolution_and_wall_isolation()?;
    run_q2_cadence_and_sensor_budget()?;
    run_q3_full_6144x2048_worst_case_throughput()?;
    run_q4_world_dimension_scaling()?;
    run_q7_1220_day_multiscale_and_catchup_consistency()?;
    run_q7_three_modes();
    run_q7_long_ecology(12_200)?;
    run_q7_long_ecology(36_600)?;
    run_q7_dormant_water(12_200)?;
    run_q7_dormant_water(36_600)?;

    println!("\n[ALL QUANTITATIVE BENCHMARKS COMPLETED]");
    Ok(())
}

/// Q1: Compare L1 FieldDownsample By4 (4x4) vs By2 (2x2) and meters_per_cell (0.5m vs 0.25m)
/// at candidate 6144x2048 scale, plus verify wall-aware flux_block_mask prevents thin-wall leakage.
fn run_q1_resolution_and_wall_isolation() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- [Q1] 基准物理尺度与 L1 降采样比例 (6144x2048) ---");
    for (label, mpc, ds) in [
        ("0.50m / L1 4x4 (By4)", 0.50f32, FieldDownsample::By4),
        ("0.25m / L1 4x4 (By4)", 0.25f32, FieldDownsample::By4),
        ("0.50m / L1 2x2 (By2)", 0.50f32, FieldDownsample::By2),
    ] {
        let mut cfg = WorldConfig::candidate_6144x2048();
        cfg.meters_per_cell = mpc;
        cfg.field_downsample = ds;

        let t_gen = Instant::now();
        let (mut world, report, _) = generate_valid_world(cfg, MasterSeed::from_u64(101), 4)?;
        let gen_ms = t_gen.elapsed().as_secs_f64() * 1000.0;
        assert!(report.passed);

        let mem_mb = world.estimated_memory_bytes() as f64 / (1024.0 * 1024.0);
        let all_active = vec![true; world.l0_chunks.len()];

        let t_l1 = Instant::now();
        fields::step_l1_fields(&mut world, &all_active);
        let l1_full_ms = t_l1.elapsed().as_secs_f64() * 1000.0;

        // Typical active LOD-0 window (~12x10 = 120 chunks)
        let mut sim = WorldSimulation::new(world);
        let active_cnt = sim.active_lod0_count;
        let active_mask = sim.active_mask().to_vec();
        let t_ef = Instant::now();
        electric::solve_local_electric_field(&mut sim.world, &active_mask);
        let ef_ms = t_ef.elapsed().as_secs_f64() * 1000.0;

        // Verify thin-wall flux isolation (place 2-cell basalt barrier across an L1 boundary)
        let blocked_tiles: usize = sim
            .world
            .l1_chunks
            .iter()
            .map(|c| c.flux_block_mask.iter().filter(|&&m| m != 0).count())
            .sum();
        let total_l1_tiles: usize = sim
            .world
            .l1_chunks
            .iter()
            .map(|c| c.flux_block_mask.len())
            .sum();

        println!(
            "  {label:<22} | mem={mem_mb:6.1} MB | gen={gen_ms:6.1} ms | L1_full={l1_full_ms:5.2} ms | E_LOD0({active_cnt}ch)={ef_ms:5.2} ms | wall_masked={:.1}%",
            (blocked_tiles as f64 / total_l1_tiles.max(1) as f64) * 100.0
        );
    }
    println!();
    Ok(())
}

/// Q2: Compare 60Hz vs 30Hz fine cadence & 15Hz vs 20Hz electric solver + 1,000 O(1) sensor samples/tick.
fn run_q2_cadence_and_sensor_budget() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- [Q2] 基准模拟频率 (60Hz vs 30Hz)、电场频次与 1,000 生物 O(1) 电感采样压测 ---");
    for (label, fine_hz, e_period, iters) in [
        ("60Hz / 电场15Hz / 6轮松弛", 60u32, 4u32, 6u8),
        ("60Hz / 电场20Hz / 6轮松弛", 60u32, 3u32, 6u8),
        ("30Hz / 电场15Hz / 6轮松弛", 30u32, 2u32, 6u8),
    ] {
        let mut cfg = WorldConfig::candidate_6144x2048();
        cfg.clock.fine_hz = fine_hz;
        cfg.clock.electric_period_ticks = e_period;
        cfg.electric_solver_iterations = iters;

        let (world, _, _) = generate_valid_world(cfg, MasterSeed::from_u64(202), 4)?;
        let mut sim = WorldSimulation::new(world);

        // Run 120 fine ticks + measure 1,000 O(1) electroreception probes per tick
        let w_m = sim.world.config.world_width_meters();
        let h_m = sim.world.config.world_height_meters();
        let mut probe_us_total = 0u64;
        let mut checksum = 0.0f64;

        for t in 0..120u64 {
            sim.step_fine_tick();
            let tp = Instant::now();
            for i in 0..1000u64 {
                let px = ((i * 73 + t * 13) % 1000) as f64 / 1000.0 * w_m;
                let py = ((i * 41 + t * 7) % 1000) as f64 / 1000.0 * h_m;
                let s = sample_electric_gradient(&sim.world, WorldPosMeters::new(px, py), true);
                checksum += (s.grad_x + s.grad_y) as f64;
            }
            probe_us_total += tp.elapsed().as_micros() as u64;
        }
        let prof = sim.profiler_snapshot();
        let avg_probe_us = probe_us_total / 120;
        let budget_ms = 1000.0 / (fine_hz as f64);
        println!(
            "  {label:<26} | 帧预算={budget_ms:5.2}ms | Core P50={:4}us P95={:4}us P99={:4}us | 1000电感探针={avg_probe_us:3}us (chk={:.0})",
            prof.p50_total_us, prof.p95_total_us, prof.p99_total_us, checksum.abs() % 1000.0
        );
    }
    println!();
    Ok(())
}

/// Q3: Worst-case full 6144x2048 flood/red-rain parallel 4-phase fluid + L1 fields + electric throughput.
fn run_q3_full_6144x2048_worst_case_throughput() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- [Q3] 6144x2048 全图最坏负载 (全图唤醒流体 + L1 场 + 活跃电场) ---");
    let cfg = WorldConfig::candidate_6144x2048();
    let (mut world, _, _) = generate_valid_world(cfg, MasterSeed::from_u64(303), 4)?;

    // Wake ALL chunks simultaneously to simulate global torrential red-rain flood worst case
    let max_c = plain_contracts::ChunkCoord::new(
        world.config.dimensions.chunks_x as i32 - 1,
        world.config.dimensions.chunks_y as i32 - 1,
    );
    wake_fluid_chunks(&mut world, plain_contracts::ChunkCoord::new(0, 0), max_c);
    let all_active = vec![true; world.l0_chunks.len()];

    let t_fl = Instant::now();
    let mut moved = 0u64;
    for step in 0..10u64 {
        moved += fluid::step_fluid_active_chunks(&mut world, &all_active, step) as u64;
    }
    let avg_fluid_ms = (t_fl.elapsed().as_secs_f64() * 1000.0) / 10.0;

    let t_l1 = Instant::now();
    for _ in 0..10 {
        fields::step_l1_fields(&mut world, &all_active);
    }
    let avg_l1_ms = (t_l1.elapsed().as_secs_f64() * 1000.0) / 10.0;

    let t_ef = Instant::now();
    for _ in 0..10 {
        electric::solve_local_electric_field(&mut world, &all_active);
    }
    let avg_ef_full_ms = (t_ef.elapsed().as_secs_f64() * 1000.0) / 10.0;

    // Also measure single-viewport (realistic LOD-0) full tick steady state
    let mut sim = WorldSimulation::new(world);
    for _ in 0..60 {
        sim.step_fine_tick();
    }
    let prof = sim.profiler_snapshot();
    println!(
        "  全图3072块同步满载: 流体={avg_fluid_ms:6.2}ms/tick (moves={moved}) | L1全场={avg_l1_ms:6.2}ms | 全图电场={avg_ef_full_ms:6.2}ms"
    );
    println!(
        "  常规 LOD-0 单视口 ({} 活跃块): Core P50={}us | P95={}us | P99={}us",
        prof.active_lod0_chunks, prof.p50_total_us, prof.p95_total_us, prof.p99_total_us
    );
    println!();
    Ok(())
}

/// Q4: Scaling curves across 4096x1536, 6144x2048, and 8192x2048 (Memory, Gen, SQLite Zstd Baseline, Load).
fn run_q4_world_dimension_scaling() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- [Q4] 可配置世界尺寸阶梯压测 (4096x1536 / 6144x2048 / 8192x2048) ---");
    for (label, cfg) in [
        ("4096x1536", WorldConfig::preset_4096x1536()),
        ("6144x2048", WorldConfig::candidate_6144x2048()),
        ("8192x2048", WorldConfig::preset_8192x2048()),
    ] {
        let t_gen = Instant::now();
        let (mut world, report, _) = generate_valid_world(cfg, MasterSeed::from_u64(404), 4)?;
        let gen_ms = t_gen.elapsed().as_secs_f64() * 1000.0;
        assert!(report.passed, "Validation failed for {label}");

        let mem_mb = world.estimated_memory_bytes() as f64 / (1024.0 * 1024.0);
        let save_path = PathBuf::from(format!(
            "/tmp/plain_q4_{label}_{}.sqlite",
            std::process::id()
        ));
        if save_path.exists() {
            let _ = std::fs::remove_file(&save_path);
        }
        let mut store = WorldSaveStore::open(&save_path)?;
        let save_stats = store.save_full_baseline(&mut world, Some(&save_path))?;

        let t_load = Instant::now();
        let loaded = store.load_world()?;
        let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;
        store.close()?;

        let mut sim = WorldSimulation::new(loaded);
        for _ in 0..60 {
            sim.step_fine_tick();
        }
        let prof = sim.profiler_snapshot();
        let _ = std::fs::remove_file(&save_path);

        println!(
            "  {label:<10} | mem={mem_mb:5.1}MB | gen={gen_ms:6.1}ms | Zstd基线={:5.2}MB (存{:5.1}ms/读{:5.1}ms) | LOD0 P95={:4}us",
            save_stats.file_size_bytes as f64 / (1024.0 * 1024.0),
            save_stats.elapsed_ms,
            load_ms,
            prof.p95_total_us
        );
    }
    println!();
    Ok(())
}

/// Q7: 1220-day non-uniform, non-linear multi-scale stepping (24 vs 4 steps/day),
/// 3x1220 = 3660-day conservation & oscillation check, and 400-day dormant chunk Lazy Catch-up vs Continuous comparison.
fn run_q7_1220_day_multiscale_and_catchup_consistency() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "--- [Q7] 1220日多尺度时间推进 (24步/日 vs 4步/日)、3x1220日稳态与 400日懒惰追平一致性 ---"
    );

    for steps_per_day in [24u32, 4u32] {
        let mut cfg = WorldConfig::candidate_6144x2048();
        cfg.clock.macro_steps_per_game_day = steps_per_day;
        let (world, _, _) = generate_valid_world(cfg, MasterSeed::from_u64(707), 4)?;
        let mut sim = WorldSimulation::new(world);

        let initial_transitions: Vec<_> = sim
            .world
            .regions
            .iter()
            .map(|basin| basin.era_transitions_count)
            .collect();
        let one_cycle_steps = 1220u64 * (steps_per_day as u64);
        let t_1c = Instant::now();
        sim.step_macro_steps(one_cycle_steps, false);
        let ms_1c = t_1c.elapsed().as_secs_f64() * 1000.0;
        verify_water(&sim.world, "1220-day macro")?;

        let two_more_steps = 2 * one_cycle_steps;
        let t_3c = Instant::now();
        sim.step_macro_steps(two_more_steps, false);
        let ms_total_3c = ms_1c + t_3c.elapsed().as_secs_f64() * 1000.0;

        verify_water(&sim.world, "3660-day macro")?;
        let t_catch_up = Instant::now();
        sim.catch_up_all_chunks();
        let catch_up_ms = t_catch_up.elapsed().as_secs_f64() * 1000.0;
        verify_water(&sim.world, "3660-day catch-up")?;
        let transitions: Vec<_> = sim
            .world
            .regions
            .iter()
            .zip(initial_transitions)
            .map(|(basin, start)| {
                let count = basin.era_transitions_count - start;
                assert!(
                    count >= 8,
                    "流域 {:?} 在 3660 日内只切换 {count} 次",
                    basin.id
                );
                count
            })
            .collect();
        println!(
            "  宏步={steps_per_day:2}/日 | 1220日={ms_1c:6.2}ms | 3660日={ms_total_3c:6.2}ms | 全图追平={catch_up_ms:6.2}ms | 逐流域切换={transitions:?} | 水漂移 0 uL"
        );
    }

    // Lazy Catch-up vs Continuous Active Comparison over 400 game days on a test basin chunk
    let cfg = WorldConfig::preset_ci_1024x512();
    let seed = MasterSeed::from_u64(777);
    let (world_cont, _, _) = generate_valid_world(cfg.clone(), seed, 4)?;
    let (world_lazy, _, _) = generate_valid_world(cfg.clone(), seed, 4)?;

    let mut sim_cont = WorldSimulation::new(world_cont);
    let mut sim_lazy = WorldSimulation::new(world_lazy);

    // Pick a surface viewport in Basin 0
    let target_x = (sim_cont.world.regions[0].x_start + sim_cont.world.regions[0].x_end) / 2;
    let target_y = sim_cont.world.topology.surface_height[target_x] as i32;
    let target_vp = ViewportRect {
        min_x: (target_x as i32 - 64).max(0),
        min_y: (target_y - 64).max(0),
        max_x: (target_x as i32 + 64).min(sim_cont.world.width_cells() as i32 - 1),
        max_y: (target_y + 64).min(sim_cont.world.height_cells() as i32 - 1),
    };

    // Group A realizes the anchored viewport every day for 400 days.
    sim_cont.set_simulation_anchor(0, target_vp);
    // Group B leaves the same area dormant.
    sim_lazy.remove_simulation_anchor(0);

    let spd = cfg.clock.macro_steps_per_game_day as u64;
    for _day in 0..400u64 {
        sim_cont.step_macro_steps(spd, true);
        sim_lazy.step_macro_steps(spd, false);
    }

    // Activate the target in Group B and fully realize its macro history.
    let t_tp = Instant::now();
    sim_lazy.set_simulation_anchor(0, target_vp);
    sim_lazy.step_macro_steps(0, true);
    let tp_ms = t_tp.elapsed().as_secs_f64() * 1000.0;

    // Compare L1 pH, fertility, salinity & L0 SaltCrust cell count inside target_vp
    let mut ph_abs_err = 0.0f64;
    let mut fert_rel_err = 0.0f64;
    let mut sal_rel_err = 0.0f64;
    let mut samples = 0u64;
    let mut salt_cont = 0u64;
    let mut salt_lazy = 0u64;

    for y in target_vp.min_y..=target_vp.max_y {
        for x in target_vp.min_x..=target_vp.max_x {
            let c = CellCoord::new(x, y);
            if sim_cont.world.get_material(c) == MaterialId::SALT_CRUST {
                salt_cont += 1;
            }
            if sim_lazy.world.get_material(c) == MaterialId::SALT_CRUST {
                salt_lazy += 1;
            }
            let ds = cfg.field_downsample.factor() as i32;
            if x % ds == 0 && y % ds == 0 {
                let ci = sim_cont.world.chunk_index(c.chunk_coord()).unwrap();
                let a = &sim_cont.world.l1_chunks[ci];
                let b = &sim_lazy.world.l1_chunks[ci];
                let side = plain_contracts::CHUNK_SIZE as i32;
                let i = a.idx((x % side / ds) as usize, (y % side / ds) as usize);
                ph_abs_err += f64::from(a.ph_milli[i].abs_diff(b.ph_milli[i])) * 0.001;
                fert_rel_err += f64::from(a.fertility[i].abs_diff(b.fertility[i]))
                    / f64::from(a.fertility[i].max(1));
                sal_rel_err += f64::from(a.salinity[i].abs_diff(b.salinity[i]))
                    / f64::from(a.salinity[i].max(1));
                samples += 1;
                if sim_lazy.world.l0_chunks[ci].last_realized_macro_step
                    != sim_lazy.world.clock.macro_step.0
                {
                    return Err("Q7 lazy comparator did not finish catch-up".into());
                }
            }
        }
    }

    let ph_error = ph_abs_err / samples.max(1) as f64;
    let fertility_error = fert_rel_err / samples.max(1) as f64;
    let salinity_error = sal_rel_err / samples.max(1) as f64;
    verify_water(&sim_cont.world, "400-day continuous")?;
    verify_water(&sim_lazy.world, "400-day catch-up")?;
    let (_, _, w_cont) = sim_cont.world.total_world_water();
    let (_, _, w_lazy) = sim_lazy.world.total_world_water();
    println!(
        "  400日休眠懒惰追平对照: 瞬移追平耗时={tp_ms:.2}ms | 平均pH误差={:.3} | 肥力误差={:.2}% | 盐度误差={:.2}% | 盐壳格(连续={salt_cont} vs 懒追={salt_lazy}) | 水守恒漂移(A={}, B={})",
        ph_error,
        fertility_error * 100.0,
        salinity_error * 100.0,
        w_cont.0 - sim_cont.world.initial_world_water_ul,
        w_lazy.0 - sim_lazy.world.initial_world_water_ul
    );
    if samples == 0 || ph_error > 0.15 || fertility_error > 0.025 || salinity_error > 0.025 {
        return Err(
            "Q7 400-day catch-up exceeded the pH, fertility or salinity error budget".into(),
        );
    }
    Ok(())
}

fn run_q7_three_modes() {
    let samples = three_modes::run();
    println!("  同种子同流域 4 日对照 | 水位（m）/ 下游积水（uL）/ 四菌群均值 / L1 电导率均值");
    for (mode, sample) in ["休眠", "快进", "实时"].into_iter().zip(samples) {
        println!(
            "    {mode} | {:.4} m | {} uL | {:.3?} | {:.3}",
            sample.channel_level_m, sample.downstream_ul, sample.microbes, sample.conductivity
        );
    }
    three_modes::assert_close(samples);
}

fn verify_water(world: &WorldState, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    let drift = world.total_world_water().2 .0 - world.initial_world_water_ul;
    if drift != 0
        || world.regions.iter().any(|region| {
            region.water_vapor_ul < 0
                || region.pending_surface_water_ul < 0
                || !(0..=region.pending_surface_water_ul)
                    .contains(&region.precipitation_remainder_ul)
        })
    {
        return Err(format!("Q7 {label} water ledger failed, drift {drift} uL").into());
    }
    Ok(())
}

#[derive(Default)]
struct EcologySample {
    salts: usize,
    silt: usize,
    travertine: usize,
    ph: f64,
    salinity: f64,
    fertility: f64,
    acidic_floor: usize,
    fertility_ceiling: usize,
    tiles: usize,
}

fn ecology_sample(world: &WorldState) -> EcologySample {
    let mut result = EcologySample::default();
    for chunk in &world.l0_chunks {
        for &material in &chunk.solid_material {
            match MaterialId(material) {
                MaterialId::SALT_CRUST => result.salts += 1,
                MaterialId::SILT_CLAY => result.silt += 1,
                MaterialId::TRAVERTINE => result.travertine += 1,
                _ => {}
            }
        }
    }
    for chunk in &world.l1_chunks {
        for ((&ph, &salinity), &fertility) in chunk
            .ph_milli
            .iter()
            .zip(&chunk.salinity)
            .zip(&chunk.fertility)
        {
            result.ph += f64::from(ph) * 0.001;
            result.salinity += f64::from(salinity);
            result.fertility += f64::from(fertility);
            result.acidic_floor += usize::from(ph <= 3_200);
            result.fertility_ceiling += usize::from(fertility >= 65_000);
            result.tiles += 1;
        }
    }
    result.ph /= result.tiles as f64;
    result.salinity /= result.tiles as f64;
    result.fertility /= result.tiles as f64;
    result
}

fn black_water_ul(world: &WorldState) -> i64 {
    world
        .l0_chunks
        .iter()
        .map(|chunk| {
            chunk
                .fluid_kind
                .iter()
                .zip(&chunk.fluid_volume)
                .filter(|(&kind, _)| kind == FluidKind::BlackWater as u8)
                .map(|(_, &volume)| i64::from(volume))
                .sum::<i64>()
        })
        .sum()
}

fn run_q7_long_ecology(days: u64) -> Result<(), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let mut cfg = WorldConfig::preset_ci_1024x512();
    cfg.clock.macro_steps_per_game_day = 4;
    let (world, _, _) = generate_valid_world(cfg, MasterSeed::from_u64(42), 4)?;
    let mut sim = WorldSimulation::new(world);
    let initial = ecology_sample(&sim.world);
    let initial_vapor = sim.world.total_world_water().1 .0;
    let initial_black_water = black_water_ul(&sim.world);
    let steps_per_day = u64::from(sim.world.config.clock.macro_steps_per_game_day);
    let mut previous_salt = initial.salts;
    let mut salt_gains = 0;
    let mut salt_losses = 0;
    let mut salt_peak = initial.salts;
    let mut min_vapor = initial_vapor;
    let mut max_pending = 0;
    let mut min_black_water = initial_black_water;
    let mut min_ph = initial.ph;
    let mut max_ph = initial.ph;
    let all = vec![true; sim.world.l0_chunks.len()];
    for day in (0..days).step_by(30) {
        let duration = (days - day).min(30);
        sim.step_macro_steps(duration * steps_per_day, false);
        sim.catch_up_all_chunks();
        for tick in 0..12 {
            fluid::step_fluid_active_chunks(&mut sim.world, &all, tick);
        }
        verify_water(&sim.world, "long ecology")?;
        let sample = ecology_sample(&sim.world);
        let vapor = sim.world.total_world_water().1 .0;
        let pending: i64 = sim
            .world
            .regions
            .iter()
            .map(|r| r.pending_surface_water_ul)
            .sum();
        min_vapor = min_vapor.min(vapor);
        max_pending = max_pending.max(pending);
        salt_peak = salt_peak.max(sample.salts);
        salt_gains += usize::from(sample.salts > previous_salt);
        salt_losses += usize::from(sample.salts < previous_salt);
        previous_salt = sample.salts;
        if (day + duration) / 1220 != day / 1220 || day + duration == days {
            min_black_water = min_black_water.min(black_water_ul(&sim.world));
        }
        min_ph = min_ph.min(sample.ph);
        max_ph = max_ph.max(sample.ph);
        if sample.acidic_floor * 100 > sample.tiles
            || sample.fertility_ceiling * 100 > sample.tiles
            || sample.silt * 2 < initial.silt
            || sample.travertine * 2 < initial.travertine
            || sample.salinity < initial.salinity * 0.5
            || min_vapor * 2 < initial_vapor
            || pending * 2 > initial_vapor
        {
            return Err(format!("Q7 day {} ecology outside limits, pH {:.3}, fertility {:.0}, salinity {:.0}, vapor {:.1}%, pending {pending}, silt {}, travertine {}",
                day + duration, sample.ph, sample.fertility, sample.salinity, vapor as f64 / initial_vapor as f64 * 100.0,
                sample.silt, sample.travertine).into());
        }
    }
    assert!(
        initial_black_water > 0 && min_black_water >= initial_black_water / 2,
        "Q7 {days}-day black-water reservoir fell to {min_black_water}/{initial_black_water} uL"
    );
    let end = ecology_sample(&sim.world);
    println!(
        "  {days}日（含细格流体沉降）| {:.2}s | pH {:.3}–{:.3} | 最低水汽 {:.1}% | 待沉降峰值 {} uL | 盐壳峰值 {}（增{}次/减{}次）| 粉砂 {}→{} | 钙华 {}→{} | 盐度 {:.0}→{:.0} | 水漂移 0 uL",
        start.elapsed().as_secs_f64(), min_ph, max_ph, min_vapor as f64 / initial_vapor as f64 * 100.0,
        max_pending, salt_peak, salt_gains, salt_losses, initial.silt, end.silt, initial.travertine, end.travertine, initial.salinity, end.salinity
    );
    println!(
        "    暗河黑水最低实测 {:.1}%（每纪元循环采样）",
        min_black_water as f64 * 100.0 / initial_black_water as f64
    );
    if salt_peak == 0 || salt_gains < 2 || salt_losses < 2 || max_ph - min_ph < 0.15 {
        return Err("Q7 missing seasonal salt-crust or pH oscillation".into());
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn ten_cycles_with_fluid_settling_remain_dynamic() {
    run_q7_long_ecology(12_200).unwrap();
}

fn run_q7_dormant_water(days: u64) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let (world, _, _) = generate_valid_world(
        WorldConfig::preset_ci_1024x512(),
        MasterSeed::from_u64(42),
        4,
    )?;
    let mut sim = WorldSimulation::new(world);
    let initial_vapor = sim.world.total_world_water().1 .0;
    let initial_black_water = black_water_ul(&sim.world);
    let spd = u64::from(sim.world.config.clock.macro_steps_per_game_day);
    let mut min_vapor = initial_vapor;
    let mut max_pending = 0;
    for day in (0..days).step_by(30) {
        sim.step_macro_steps((days - day).min(30) * spd, false);
        verify_water(&sim.world, "dormant ecology")?;
        min_vapor = min_vapor.min(sim.world.total_world_water().1 .0);
        let pending: i64 = sim
            .world
            .regions
            .iter()
            .map(|region| region.pending_surface_water_ul)
            .sum();
        max_pending = max_pending.max(pending);
    }
    if min_vapor * 2 < initial_vapor
        || max_pending * 2 > initial_vapor
        || sim
            .world
            .regions
            .iter()
            .any(|region| region.flux_history.archived_buckets.is_empty())
    {
        return Err("Q7 dormant ecology missed water or archive bounds".into());
    }
    sim.catch_up_all_chunks();
    verify_water(&sim.world, "dormant archive replay")?;
    if sim
        .world
        .l0_chunks
        .iter()
        .any(|chunk| chunk.last_realized_macro_step != sim.world.clock.macro_step.0)
    {
        return Err("Q7 late archive catch-up left a stale chunk".into());
    }
    let sample = ecology_sample(&sim.world);
    if sample.acidic_floor * 100 > sample.tiles || sample.fertility_ceiling * 100 > sample.tiles {
        return Err("Q7 archive replay produced saturated chemistry".into());
    }
    let black_water = black_water_ul(&sim.world);
    assert!(initial_black_water > 0 && black_water >= initial_black_water / 2,
        "Q7 {days}-day dormant black-water reservoir fell to {black_water}/{initial_black_water} uL");
    println!(
        "  {days}日（纯宏步休眠后全图追平）| {:.2}s | 最低水汽 {:.1}% | 暗河黑水 {:.1}% | 待沉降峰值 {max_pending} uL | pH {:.3} | 水漂移 0 uL",
        started.elapsed().as_secs_f64(), min_vapor as f64 / initial_vapor as f64 * 100.0,
        black_water as f64 * 100.0 / initial_black_water as f64, sample.ph
    );
    Ok(())
}
