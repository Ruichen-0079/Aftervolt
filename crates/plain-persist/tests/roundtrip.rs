use plain_contracts::{MasterSeed, WorldConfig};
use plain_persist::{world_state_hash, WorldSaveStore};
use plain_world::WorldSimulation;
use plain_worldgen::generate_world;

#[test]
fn sqlite_baseline_and_incremental_roundtrip_is_bit_exact() {
    let cfg = WorldConfig::preset_ci_1024x512();
    let seed = MasterSeed::from_u64(9001);
    let (mut world, report) = generate_world(cfg.clone(), seed).unwrap();
    assert!(report.passed, "{}", report.failure_summary());

    // Determinism check against a fresh generation
    let (world_dup, _) = generate_world(cfg, seed).unwrap();
    let h_genesis = world_state_hash(&world);
    assert_eq!(
        h_genesis,
        world_state_hash(&world_dup),
        "Genesis must be bit-identical across runs"
    );

    let tmp_path =
        std::env::temp_dir().join(format!("plain-roundtrip-{}.sqlite", std::process::id()));
    if tmp_path.exists() {
        let _ = std::fs::remove_file(&tmp_path);
    }

    let mut store = WorldSaveStore::open(&tmp_path).unwrap();
    store
        .save_full_baseline(&mut world, Some(&tmp_path))
        .unwrap();
    let loaded_base = store.load_world().unwrap();
    assert_eq!(
        h_genesis,
        world_state_hash(&loaded_base),
        "Loaded SQLite baseline must match genesis bit-for-bit"
    );

    // Mutate world with fine ticks + 200 days macro + catch-up, then incremental save
    let mut sim = WorldSimulation::new(loaded_base);
    sim.world.regions[0].flux_history.max_len = 128;
    for _ in 0..45 {
        sim.step_fine_tick();
    }
    let steps_200d = 200u64 * (sim.world.config.clock.macro_steps_per_game_day as u64);
    sim.step_macro_steps(steps_200d, true);

    let h_mutated = world_state_hash(&sim.world);
    let stats = store
        .save_incremental(&mut sim.world, Some(&tmp_path))
        .unwrap();
    assert!(stats.dirty_chunks_written > 0);

    let loaded_mut = store.load_world().unwrap();
    assert_eq!(
        h_mutated,
        world_state_hash(&loaded_mut),
        "Loaded incremental SQLite state must match mutated world bit-for-bit"
    );

    assert!(!loaded_mut.regions[0]
        .flux_history
        .archived_buckets
        .is_empty());
    store.close().unwrap();
    for suffix in ["-wal", "-shm"] {
        let mut companion = tmp_path.as_os_str().to_os_string();
        companion.push(suffix);
        assert!(!std::path::Path::new(&companion).exists());
    }
    let reopened = WorldSaveStore::open(&tmp_path).unwrap();
    assert_eq!(world_state_hash(&reopened.load_world().unwrap()), h_mutated);
    reopened.close().unwrap();
    std::fs::remove_file(&tmp_path).unwrap();
}

#[test]
fn hundred_seed_legality_sweep_passes_100_percent() {
    let cfg = WorldConfig::preset_ci_1024x512();
    let mut failures = Vec::new();
    for s in 1..=100u64 {
        let (_, report) = generate_world(cfg.clone(), MasterSeed::from_u64(s)).unwrap();
        if !report.passed {
            failures.push((s, report.failure_summary()));
        }
    }
    assert!(
        failures.is_empty(),
        "Failed {}/100 seeds: {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

#[test]
fn hash_covers_flux_record_contents_and_authoritative_metadata() {
    let (world, _) = generate_world(
        WorldConfig::preset_ci_1024x512(),
        MasterSeed::from_u64(9001),
    )
    .unwrap();
    let mut sim = WorldSimulation::new(world);
    sim.step_macro_steps(1, false);
    let world = sim.world;
    let original = world_state_hash(&world);

    let mut changed = world.clone();
    changed.regions[0].flux_history.records[0].fertility_delta += 1.0;
    assert_eq!(
        changed.regions[0].flux_history.records.len(),
        world.regions[0].flux_history.records.len()
    );
    assert_ne!(world_state_hash(&changed), original);

    let mut changed = world.clone();
    changed.regions[0].evaporation_remainder_ul += 1;
    assert_ne!(world_state_hash(&changed), original);

    let mut changed = world.clone();
    changed.regions[0].moisture_index *= 0.5;
    assert_ne!(world_state_hash(&changed), original);

    let mut changed = world.clone();
    changed.topology.ruin_anchors[0].waste_heat_bonus_dk += 1;
    assert_ne!(world_state_hash(&changed), original);

    let mut changed = world.clone();
    changed.config.clock.fine_hz += 1;
    assert_ne!(world_state_hash(&changed), original);
}

#[test]
fn hash_canonicalizes_float_zero_and_nan_payloads() {
    let (world, _) = generate_world(
        WorldConfig::preset_ci_1024x512(),
        MasterSeed::from_u64(9001),
    )
    .unwrap();
    let mut sim = WorldSimulation::new(world);
    sim.step_macro_steps(1, false);
    let mut a = sim.world;
    let mut b = a.clone();
    a.l1_chunks[0].electric_potential[0] = 0.0;
    b.l1_chunks[0].electric_potential[0] = -0.0;
    a.regions[0].flux_history.records[0].fertility_delta = f32::NAN;
    b.regions[0].flux_history.records[0].fertility_delta = f32::from_bits(0xffc0_0001);
    a.topology.natural_flower_sea_column_ratio = -0.0;
    b.topology.natural_flower_sea_column_ratio = 0.0;
    assert_eq!(world_state_hash(&a), world_state_hash(&b));
}

#[test]
fn sqlite_loads_v1_water_ledger_without_replaying_precredited_evaporation() {
    use plain_contracts::{
        CellCoord, EraPhase, FluidKind, RegionId, WeatherSpectacle, WorldDimensions,
    };
    use plain_persist::schema::{MacroRegionV1, RegionalMacroFluxHistoryV1};
    use plain_persist::{VersionedWorldMeta, WorldMetaV1};
    use plain_world::WorldState;

    let config = WorldConfig {
        dimensions: WorldDimensions::from_chunks(1, 1),
        ..WorldConfig::default()
    };
    let mut world = WorldState::new_empty(config.clone(), MasterSeed::from_u64(1), 1);
    let cell = CellCoord::new(0, 1);
    world.set_fluid_gen(cell, FluidKind::RainPool, 20);
    world.initial_world_water_ul = 17;
    world.clock.advance_macro_steps(1);
    let path =
        std::env::temp_dir().join(format!("plain-test-water-v1-{}.sqlite", std::process::id()));
    let mut store = WorldSaveStore::open(&path).unwrap();
    store.save_full_baseline(&mut world, None).unwrap();

    let legacy = VersionedWorldMeta::V1(WorldMetaV1 {
        schema_version: 1,
        worldgen_version: 1,
        seed: world.seed,
        config,
        clock: world.clock,
        initial_world_water_ul: world.initial_world_water_ul,
        regions: vec![MacroRegionV1 {
            id: RegionId(0),
            name: "Basin".into(),
            role: plain_world::BasinRole::RidgePlateau,
            x_start: 0,
            x_end: 64,
            mean_surface_y: 0,
            water_vapor_ul: 5,
            pending_surface_water_ul: -8,
            mineral_dust_units: 0,
            surface_charge_uc: 0,
            moisture_index: 0.5,
            fertility_index: 0.5,
            salt_crust_ratio: 0.0,
            charge_index: 0.0,
            dust_saturation: 0.0,
            flower_sea_suitability: 0.5,
            era: EraPhase::FlowerSeaBloom,
            weather: WeatherSpectacle::Calm,
            era_elapsed_macro_steps: 1,
            era_transitions_count: 0,
            flux_history: RegionalMacroFluxHistoryV1 {
                records: vec![plain_persist::schema::MacroFluxRecordV2 {
                    macro_step: 1,
                    net_water_delta_per_col: -1,
                    fertility_delta: -1.0,
                    ..plain_persist::schema::MacroFluxRecordV2::default()
                }],
                max_len: 64,
            },
        }],
        topology: world.topology.clone(),
    });
    let raw = postcard::to_allocvec(&legacy).unwrap();
    let compressed = zstd::bulk::compress(&raw, 1).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE world_meta SET value = ?1 WHERE key = 'world_meta_v1'",
        [compressed],
    )
    .unwrap();
    drop(conn);

    let mut loaded = store.load_world().unwrap();
    assert_eq!(loaded.worldgen_version, 1);
    assert_eq!(loaded.total_world_water().2 .0, 17);
    assert_eq!(loaded.get_fluid(cell).1, 17);
    assert_eq!(loaded.regions[0].water_vapor_ul, 0);
    assert_eq!(loaded.regions[0].pending_surface_water_ul, 0);
    let record = &loaded.regions[0].flux_history.records[0];
    assert_eq!(record.net_water_delta_per_col, 0);
    assert_eq!(record.fertility_delta, -1.0);

    store.save_incremental(&mut loaded, None).unwrap();
    assert_eq!(
        world_state_hash(&store.load_world().unwrap()),
        world_state_hash(&loaded)
    );
    store.close().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn developer_cameras_leave_the_full_simulation_hash_bit_exact() {
    use plain_contracts::ViewportRect;
    let (world, _) =
        generate_world(WorldConfig::preset_ci_1024x512(), MasterSeed::from_u64(42)).unwrap();
    let mut watched = WorldSimulation::new(world.clone());
    let mut control = WorldSimulation::new(world);

    for i in 0..8 {
        let x = i * 100;
        watched.set_dev_camera(
            0,
            ViewportRect {
                min_x: x,
                min_y: 10,
                max_x: x + 100,
                max_y: 110,
            },
        );
        assert_eq!(watched.active_mask(), control.active_mask());
        for simulation in [&mut watched, &mut control] {
            simulation.step_macro_steps(729, true);
            for _ in 0..30 {
                simulation.step_fine_tick();
            }
        }
        assert_eq!(
            world_state_hash(&watched.world),
            world_state_hash(&control.world)
        );
        if i % 2 == 1 {
            watched.remove_dev_camera(0);
        }
        assert_eq!(watched.active_mask(), control.active_mask());
        assert_eq!(
            world_state_hash(&watched.world),
            world_state_hash(&control.world)
        );
    }
}

#[test]
fn sqlite_loads_v2_meta_and_v1_chunks_with_milli_ph() {
    use plain_contracts::{EraPhase, RegionId, WeatherSpectacle, WorldDimensions};
    use plain_persist::schema::{
        ChunkBlobV1, FieldChunkV1, MacroRegionV2, RegionalMacroFluxHistoryV2, VersionedChunkBlob,
        VersionedWorldMeta, WorldMetaV2,
    };
    use plain_world::{BasinRole, WorldState};

    let mut config = WorldConfig::preset_ci_1024x512();
    config.dimensions = WorldDimensions::from_chunks(1, 1);
    let mut world = WorldState::new_empty(config, MasterSeed::from_u64(70), 2);
    world.topology.surface_height = vec![23; 64];
    world.clock.advance_macro_steps(9);
    world.initial_world_water_ul = 14;
    let path = std::env::temp_dir().join(format!("plain-legacy-v2-{}.sqlite", std::process::id()));
    let mut store = WorldSaveStore::open(&path).unwrap();
    store.save_full_baseline(&mut world, None).unwrap();

    let metadata = VersionedWorldMeta::V2(WorldMetaV2 {
        schema_version: 2,
        worldgen_version: 2,
        seed: world.seed,
        config: world.config.clone(),
        clock: world.clock,
        initial_world_water_ul: 14,
        topology: world.topology.clone(),
        regions: vec![MacroRegionV2 {
            id: RegionId(0),
            name: "Legacy basin".into(),
            role: BasinRole::RidgePlateau,
            x_start: 0,
            x_end: 64,
            mean_surface_y: 23,
            water_vapor_ul: 10,
            pending_surface_water_ul: 4,
            precipitation_remainder_ul: 3,
            evaporation_remainder_ul: 2,
            mineral_dust_units: 0,
            surface_charge_uc: 0,
            moisture_index: 0.5,
            fertility_index: 0.5,
            salt_crust_ratio: 0.0,
            charge_index: 0.0,
            dust_saturation: 0.0,
            flower_sea_suitability: 0.5,
            era: EraPhase::FlowerSeaBloom,
            weather: WeatherSpectacle::Calm,
            era_elapsed_macro_steps: 9,
            era_transitions_count: 0,
            flux_history: RegionalMacroFluxHistoryV2 {
                max_len: 8,
                discarded_through: 8,
                records: vec![plain_persist::schema::MacroFluxRecordV2 {
                    macro_step: 9,
                    ph_delta: 1.5,
                    ..plain_persist::schema::MacroFluxRecordV2::default()
                }],
            },
        }],
    });
    let l1 = world.l1_chunks.remove(0);
    let chunk = VersionedChunkBlob::V1(ChunkBlobV1 {
        l0: world.l0_chunks.remove(0),
        l1: FieldChunkV1 {
            ph_scaled: vec![73; l1.ph_milli.len()],
            axis_len: l1.axis_len,
            fertility: l1.fertility,
            salinity: l1.salinity,
            temperature_dk: l1.temperature_dk,
            microbe_saprotroph: l1.microbe_saprotroph,
            microbe_conductive: l1.microbe_conductive,
            microbe_halophile: l1.microbe_halophile,
            microbe_ferro: l1.microbe_ferro,
            electric_potential: l1.electric_potential,
            conductivity: l1.conductivity,
            flux_block_mask: l1.flux_block_mask,
        },
    });
    let conn = rusqlite::Connection::open(&path).unwrap();
    for (sql, data) in [
        (
            "UPDATE world_meta SET value = ?1 WHERE key = 'world_meta_v1'",
            postcard::to_allocvec(&metadata).unwrap(),
        ),
        (
            "UPDATE world_baseline SET data = ?1",
            postcard::to_allocvec(&chunk).unwrap(),
        ),
    ] {
        conn.execute(sql, [zstd::bulk::compress(&data, 1).unwrap()])
            .unwrap();
    }
    conn.close().unwrap();

    let mut loaded = store.load_world().unwrap();
    assert_eq!(loaded.l1_chunks[0].ph_milli[0], 7_300);
    assert_eq!(loaded.l0_chunks[0].last_realized_macro_step, 8);
    assert_eq!(loaded.regions[0].precipitation_remainder_ul, 3);
    assert_eq!(loaded.regions[0].evaporation_remainder_ul, 2);
    assert_eq!(loaded.total_world_water().2 .0, 14);
    assert!(plain_world::catchup::catch_up_chunk(&mut loaded, 0));
    assert_eq!(loaded.l0_chunks[0].last_realized_macro_step, 9);
    assert!(loaded.l1_chunks[0].ph_milli[0] > 7_300);
    store.save_incremental(&mut loaded, None).unwrap();
    assert_eq!(
        world_state_hash(&store.load_world().unwrap()),
        world_state_hash(&loaded)
    );
    store.close().unwrap();
    std::fs::remove_file(path).unwrap();
}
