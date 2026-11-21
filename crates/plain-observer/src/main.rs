#![deny(clippy::todo, clippy::unimplemented)]

//! F9 Lightweight Real-Time Developer Observer (Engineering Oscilloscope) & Headless CLI Runner.
//!
//! Strictly consumes `DevTelemetryPort` (god-view diagnostic port isolated from future player perception).
//! Supports:
//! 1. Global map & pan/zoom inspection with L3 basins, sinkholes & ruin geological anchors
//! 2. 14 environment/field overlays with same-viewport potential and conductivity comparison
//! 3. Conservation & population telemetry plots (Stage 1 conservation curves; Stage 2 population slot ready)
//! 4. Cell & individual probe inspector
//! 5. Time control (Pause/Play, Step Fine/Macro, Speed, +30/+365/+1220d Fast-Forward, Era Breakpoint)
//! 6. Real-time subsystem profiler (P50/P95/P99, memory, chunk sleep ratios)

mod gui;

use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};
use plain_contracts::{
    DevTelemetryPort, FieldDownsample, MasterSeed, OverlayKind, WorldConfig, WorldDimensions,
};
use plain_persist::{hash_to_hex, world_state_hash, WorldSaveStore};
use plain_world::WorldSimulation;
use plain_worldgen::{generate_valid_world, generate_world};

#[derive(Parser, Debug)]
#[command(name = "plain-observer", about = "Electroruin Plain Developer Observer & Stage 1 Validator")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// World seed (integer) for default GUI launch
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// Width in chunks (each chunk = 64 cells; default 96 = 6144 cells)
    #[arg(long, default_value_t = 96)]
    chunks_x: u16,

    /// Height in chunks (each chunk = 64 cells; default 32 = 2048 cells)
    #[arg(long, default_value_t = 32)]
    chunks_y: u16,

    /// Load an existing SQLite save file instead of generating a new world
    #[arg(long)]
    load: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run automated N-seed world legality validation sweep (Stage 1 DoD: 100 seeds 100% pass)
    ValidateSeeds {
        #[arg(long, default_value_t = 100)]
        count: u64,
        #[arg(long, default_value_t = 16)]
        chunks_x: u16,
        #[arg(long, default_value_t = 8)]
        chunks_y: u16,
        #[arg(long, default_value_t = 1)]
        start_seed: u64,
    },
    /// Run headless multi-cycle (default 3 x 1220 = 3660 game days) conservation & 4-era oscillation test
    SimCycles {
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value_t = 96)]
        chunks_x: u16,
        #[arg(long, default_value_t = 32)]
        chunks_y: u16,
        #[arg(long, default_value_t = 3660)]
        days: u64,
    },
    /// Verify bit-exact determinism & SQLite WAL baseline + incremental dirty-chunk roundtrip
    VerifyPersistence {
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value_t = 96)]
        chunks_x: u16,
        #[arg(long, default_value_t = 32)]
        chunks_y: u16,
        #[arg(long, default_value = "/tmp/plain_stage1_roundtrip.sqlite")]
        path: PathBuf,
    },
}

fn build_config(chunks_x: u16, chunks_y: u16) -> WorldConfig {
    let mut cfg = WorldConfig::candidate_6144x2048();
    cfg.dimensions = WorldDimensions::from_chunks(chunks_x, chunks_y);
    cfg.basin_count = if chunks_x >= 120 {
        5
    } else if chunks_x >= 80 {
        4
    } else {
        3
    };
    cfg.field_downsample = FieldDownsample::By4;
    cfg
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::ValidateSeeds {
            count,
            chunks_x,
            chunks_y,
            start_seed,
        }) => run_validate_seeds(start_seed, count, chunks_x, chunks_y),
        Some(Commands::SimCycles {
            seed,
            chunks_x,
            chunks_y,
            days,
        }) => run_sim_cycles(seed, chunks_x, chunks_y, days),
        Some(Commands::VerifyPersistence {
            seed,
            chunks_x,
            chunks_y,
            path,
        }) => run_verify_persistence(seed, chunks_x, chunks_y, path),
        None => {
            let world = if let Some(save_path) = cli.load {
                println!("Loading SQLite world from {:?}...", save_path);
                let store = WorldSaveStore::open(&save_path)?;
                store.load_world()?
            } else {
                let cfg = build_config(cli.chunks_x, cli.chunks_y);
                let seed = MasterSeed::from_u64(cli.seed);
                println!(
                    "Generating {}x{} world (seed={})...",
                    cfg.dimensions.width_cells(),
                    cfg.dimensions.height_cells(),
                    cli.seed
                );
                let t0 = Instant::now();
                let (w, report, attempt) = generate_valid_world(cfg, seed, 8)?;
                println!(
                    "Generated valid world in {:.1} ms (attempt={}, passed={})",
                    t0.elapsed().as_secs_f64() * 1000.0,
                    attempt,
                    report.passed
                );
                w
            };
            let sim = WorldSimulation::new(world);
            // Warm up 1 overview render check
            let _ = sim.render_world_overview(256, 128, OverlayKind::CompositeTerrainFluid);
            gui::run_gui(sim)?;
            Ok(())
        }
    }
}

fn run_validate_seeds(
    start_seed: u64,
    count: u64,
    chunks_x: u16,
    chunks_y: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = build_config(chunks_x, chunks_y);
    let t0 = Instant::now();
    let mut passed = 0u64;
    let mut failures = Vec::new();

    for offset in 0..count {
        let s = start_seed + offset;
        let (world, report) = generate_world(cfg.clone(), MasterSeed::from_u64(s))?;
        if report.passed {
            passed += 1;
        } else {
            failures.push((s, report.failure_summary()));
        }
        if offset == 0 {
            let h1 = world_state_hash(&world);
            let (world2, _) = generate_world(cfg.clone(), MasterSeed::from_u64(s))?;
            let h2 = world_state_hash(&world2);
            assert_eq!(h1, h2, "World generation determinism violation on seed {s}");
            println!(
                "[Determinism Check] Seed {s} bit-exact BLAKE3 = {}",
                hash_to_hex(&h1)
            );
        }
    }

    let elapsed = t0.elapsed().as_secs_f64();
    println!(
        "[ValidateSeeds] {passed}/{count} passed ({:.1}%) at {}x{} in {:.2}s ({:.1} ms/seed)",
        (passed as f64 / count.max(1) as f64) * 100.0,
        cfg.dimensions.width_cells(),
        cfg.dimensions.height_cells(),
        elapsed,
        (elapsed * 1000.0) / (count.max(1) as f64)
    );
    if !failures.is_empty() {
        for (s, msg) in failures.iter().take(10) {
            eprintln!("  FAIL seed={s}: {msg}");
        }
        return Err(format!("{} seeds failed validation", failures.len()).into());
    }
    Ok(())
}

fn run_sim_cycles(
    seed: u64,
    chunks_x: u16,
    chunks_y: u16,
    days: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = build_config(chunks_x, chunks_y);
    let steps_per_day = cfg.clock.macro_steps_per_game_day as u64;
    let total_macro_steps = days * steps_per_day;

    let (world, report, _) = generate_valid_world(cfg, MasterSeed::from_u64(seed), 4)?;
    assert!(report.passed);
    let mut sim = WorldSimulation::new(world);
    let initial_water = sim.world.initial_world_water_ul;

    let t0 = Instant::now();
    let transitions = sim.step_macro_steps(total_macro_steps, false);
    let macro_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // Catch up all chunks after multi-year evolution to verify lazy catch-up conservation
    let t_cu = Instant::now();
    sim.catch_up_all_chunks();
    let cu_ms = t_cu.elapsed().as_secs_f64() * 1000.0;

    let ledger = sim.conservation_ledger();
    let water_drift = ledger.total_world_water.0 - initial_water;

    println!(
        "[SimCycles] {} days ({} macro steps) finished in {:.1} ms (full-world catchup {:.1} ms)",
        days, total_macro_steps, macro_ms, cu_ms
    );
    println!(
        "  Water conservation: initial={} current={} drift={} uL",
        initial_water, ledger.total_world_water.0, water_drift
    );
    println!("  Total era transitions across basins: {}", transitions);
    for r in &sim.world.regions {
        println!(
            "  - {} | transitions={} | current={:?} ({:?}) | moisture={:.2} salt={:.2} fert={:.2}",
            r.name,
            r.era_transitions_count,
            r.era,
            r.weather,
            r.moisture_index,
            r.salt_crust_ratio,
            r.fertility_index
        );
    }

    if water_drift != 0 {
        return Err(format!("Water conservation violated! drift = {water_drift}").into());
    }
    if transitions < (sim.world.regions.len() as u32) * 4 {
        return Err(format!(
            "Four-era macro oscillation stalled: only {transitions} transitions over {days} days"
        )
        .into());
    }
    Ok(())
}

fn run_verify_persistence(
    seed: u64,
    chunks_x: u16,
    chunks_y: u16,
    path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    let cfg = build_config(chunks_x, chunks_y);
    let (mut world, report, _) = generate_valid_world(cfg, MasterSeed::from_u64(seed), 4)?;
    assert!(report.passed);

    let hash_genesis = world_state_hash(&world);
    let mut store = WorldSaveStore::open(&path)?;
    let base_stats = store.save_full_baseline(&mut world, Some(&path))?;
    println!(
        "[Persist Baseline] chunks={} compressed={:.2} MB file={:.2} MB time={:.1} ms",
        base_stats.baseline_chunks_written,
        base_stats.compressed_bytes_written as f64 / (1024.0 * 1024.0),
        base_stats.file_size_bytes as f64 / (1024.0 * 1024.0),
        base_stats.elapsed_ms
    );

    let t_load0 = Instant::now();
    let loaded_base = store.load_world()?;
    let load_base_ms = t_load0.elapsed().as_secs_f64() * 1000.0;
    let hash_loaded_base = world_state_hash(&loaded_base);
    assert_eq!(
        hash_genesis, hash_loaded_base,
        "Baseline save/load hash mismatch!"
    );
    println!(
        "[Persist Baseline Load] 100% bit-exact match in {:.1} ms (BLAKE3={})",
        load_base_ms,
        hash_to_hex(&hash_loaded_base)
    );

    // Advance simulation (fine ticks + 400 game days macro + visible catch-up), then incremental save
    let mut sim = WorldSimulation::new(loaded_base);
    for _ in 0..120 {
        sim.step_fine_tick();
    }
    let macro_steps_400d = 400 * (sim.world.config.clock.macro_steps_per_game_day as u64);
    sim.step_macro_steps(macro_steps_400d, true);

    let hash_after_sim = world_state_hash(&sim.world);
    let inc_stats = store.save_incremental(&mut sim.world, Some(&path))?;
    println!(
        "[Persist Incremental] dirty_chunks={} compressed={:.2} MB time={:.1} ms",
        inc_stats.dirty_chunks_written,
        inc_stats.compressed_bytes_written as f64 / (1024.0 * 1024.0),
        inc_stats.elapsed_ms
    );

    let t_load1 = Instant::now();
    let loaded_after = store.load_world()?;
    let load_inc_ms = t_load1.elapsed().as_secs_f64() * 1000.0;
    store.close()?;
    let hash_loaded_after = world_state_hash(&loaded_after);
    assert_eq!(
        hash_after_sim, hash_loaded_after,
        "Incremental save/load hash mismatch!"
    );
    println!(
        "[Persist Incremental Load] 100% bit-exact match in {:.1} ms (BLAKE3={})",
        load_inc_ms,
        hash_to_hex(&hash_loaded_after)
    );
    Ok(())
}
