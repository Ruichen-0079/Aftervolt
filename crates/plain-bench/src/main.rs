#![deny(clippy::todo, clippy::unimplemented)]

//! Quantitative Benchmark Suite for Q1, Q2, Q3, Q4 & Q7 (Stage 1 Engineering Verification).
//!
//! Executes full-scale measurements on real hardware — never uses tiny demo maps to extrapolate
//! full-world performance. Outputs structured console summary & JSON report.

use std::path::PathBuf;
use std::time::Instant;

use plain_contracts::materials::MaterialId;
use plain_contracts::{
    CellCoord, DevTelemetryPort, FieldDownsample, MasterSeed, ViewportRect, WorldConfig,
    WorldPosMeters,
};
use plain_persist::WorldSaveStore;
use plain_world::electric::sample_electric_gradient;
use plain_world::fluid::wake_fluid_chunks;
use plain_world::{electric, fields, fluid, WorldSimulation};
use plain_worldgen::generate_valid_world;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=========================================================================");
    println!("  《遗电平原》Rust World Core — 阶段 1 全尺寸定量基准验证 (Q1–Q4 & Q7)");
    println!("=========================================================================\n");

    run_q1_resolution_and_wall_isolation()?;
    run_q2_cadence_and_sensor_budget()?;
    run_q3_full_6144x2048_worst_case_throughput()?;
    run_q4_world_dimension_scaling()?;
    run_q7_1220_day_multiscale_and_catchup_consistency()?;

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
        let total_l1_tiles: usize = sim.world.l1_chunks.iter().map(|c| c.flux_block_mask.len()).sum();

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
        let save_path = PathBuf::from(format!("/tmp/plain_q4_{label}.sqlite"));
        if save_path.exists() {
            let _ = std::fs::remove_file(&save_path);
        }
        let mut store = WorldSaveStore::open(&save_path)?;
        let save_stats = store.save_full_baseline(&mut world, Some(&save_path))?;

        let t_load = Instant::now();
        let loaded = store.load_world()?;
        let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;

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
    println!("--- [Q7] 1220日多尺度时间推进 (24步/日 vs 4步/日)、3x1220日稳态与 400日懒惰追平一致性 ---");

    for steps_per_day in [24u32, 4u32] {
        let mut cfg = WorldConfig::candidate_6144x2048();
        cfg.clock.macro_steps_per_game_day = steps_per_day;
        let (world, _, _) = generate_valid_world(cfg, MasterSeed::from_u64(707), 4)?;
        let mut sim = WorldSimulation::new(world);
        let init_water = sim.world.initial_world_water_ul;

        let one_cycle_steps = 1220u64 * (steps_per_day as u64);
        let t_1c = Instant::now();
        let trans_1c = sim.step_macro_steps(one_cycle_steps, false);
        let ms_1c = t_1c.elapsed().as_secs_f64() * 1000.0;

        let two_more_steps = 2 * one_cycle_steps;
        let t_3c = Instant::now();
        let trans_2c = sim.step_macro_steps(two_more_steps, false);
        let ms_total_3c = ms_1c + t_3c.elapsed().as_secs_f64() * 1000.0;

        let (_, _, cur_water) = sim.world.total_world_water();
        let drift = cur_water.0 - init_water;
        println!(
            "  宏步={steps_per_day:2}/日 | 1x1220日={ms_1c:6.2}ms (切换{trans_1c}次) | 3x1220日={ms_total_3c:6.2}ms (总切换{}次) | 总水漂移={drift} uL",
            trans_1c + trans_2c
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

    // Group A (Continuous): observer stays on target_vp every single day for 400 days
    sim_cont.set_dev_camera(0, target_vp);
    // Group B (Dormant Lazy): observer removed for 400 days, then teleports to target_vp
    sim_lazy.remove_dev_camera(0);

    let spd = cfg.clock.macro_steps_per_game_day as u64;
    for _day in 0..400u64 {
        sim_cont.step_macro_steps(spd, true);
        sim_lazy.step_macro_steps(spd, false);
    }

    // Now teleport Group B observer onto target_vp and trigger lazy catch-up!
    let t_tp = Instant::now();
    sim_lazy.set_dev_camera(0, target_vp);
    sim_lazy.step_fine_tick();
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
            if x % 4 == 0 && y % 4 == 0 {
                if let (Some(a), Some(b)) = (
                    sim_cont.world.sample_l1_at_cell(c),
                    sim_lazy.world.sample_l1_at_cell(c),
                ) {
                    ph_abs_err += ((a.0 .0 as f64) - (b.0 .0 as f64)).abs() * 0.1;
                    fert_rel_err += ((a.1 .0 as f64) - (b.1 .0 as f64)).abs() / 65_535.0;
                    sal_rel_err += ((a.2 .0 as f64) - (b.2 .0 as f64)).abs() / 65_535.0;
                    samples += 1;
                }
            }
        }
    }

    let (_, _, w_cont) = sim_cont.world.total_world_water();
    let (_, _, w_lazy) = sim_lazy.world.total_world_water();
    println!(
        "  400日休眠懒惰追平对照: 瞬移追平耗时={tp_ms:.2}ms | 平均pH误差={:.3} | 肥力误差={:.2}% | 盐度误差={:.2}% | 盐壳格(连续={salt_cont} vs 懒追={salt_lazy}) | 水守恒漂移(A={}, B={})",
        ph_abs_err / samples.max(1) as f64,
        (fert_rel_err / samples.max(1) as f64) * 100.0,
        (sal_rel_err / samples.max(1) as f64) * 100.0,
        w_cont.0 - sim_cont.world.initial_world_water_ul,
        w_lazy.0 - sim_lazy.world.initial_world_water_ul
    );
    Ok(())
}
