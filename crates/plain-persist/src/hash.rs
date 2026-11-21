use plain_world::WorldState;

/// Hash the complete simulation state in a stable order, canonicalizing all float zeros and NaNs.
/// The save bookkeeping flag is excluded because committing a save clears it.
pub fn world_state_hash(world: &WorldState) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    macro_rules! ints {
        ($($value:expr),+ $(,)?) => { $(h.update(&($value).to_le_bytes());)+ };
    }
    macro_rules! floats {
        ($($value:expr),+ $(,)?) => { $(ints!(canonical_f32($value));)+ };
    }
    macro_rules! bytes {
        ($($value:expr),+ $(,)?) => { h.update(&[$($value as u8),+]); };
    }
    macro_rules! len {
        ($value:expr) => {
            ints!(($value).len() as u64);
        };
    }

    h.update(b"plain-world-state-hash-v3:");
    h.update(&world.seed.0);
    let config = &world.config;
    ints!(
        world.worldgen_version,
        config.dimensions.chunks_x,
        config.dimensions.chunks_y
    );
    floats!(config.meters_per_cell, config.ruin_target_fraction);
    bytes!(config.field_downsample, config.electric_solver_iterations);
    ints!(config.basin_count, config.active_lod0_padding_chunks);
    let cycle = &config.four_era_cycle;
    ints!(cycle.target_cycle_days);
    floats!(
        cycle.bloom_weight,
        cycle.desert_weight,
        cycle.reverse_rain_weight,
        cycle.red_rain_weight,
        cycle.hysteresis_margin,
        cycle.nonlinearity_exponent,
        cycle.cross_basin_wind_transfer_per_day,
        cycle.red_rain_acid_intensity,
    );
    for clock in [config.clock, world.clock.config] {
        ints!(
            clock.fine_hz,
            clock.real_seconds_per_game_day,
            clock.meso_period_ticks,
            clock.macro_steps_per_game_day,
            clock.electric_period_ticks,
        );
    }
    ints!(
        world.clock.tick.0,
        world.clock.macro_step.0,
        world.initial_world_water_ul
    );

    len!(world.l0_chunks);
    for l0 in &world.l0_chunks {
        len!(l0.solid_material);
        h.update(&l0.solid_material);
        len!(l0.fluid_kind);
        h.update(&l0.fluid_kind);
        len!(l0.solid_integrity);
        for &v in &l0.solid_integrity {
            h.update(&v.to_le_bytes());
        }
        len!(l0.fluid_volume);
        for &v in &l0.fluid_volume {
            h.update(&v.to_le_bytes());
        }
        len!(l0.cell_flags);
        h.update(&l0.cell_flags);
        len!(l0.aux_mineral);
        h.update(&l0.aux_mineral);
        let rect = l0.dirty_rect;
        let sleep = l0.sleep;
        bytes!(
            rect.min_x,
            rect.min_y,
            rect.max_x,
            rect.max_y,
            rect.active,
            sleep.fluid_sleeping,
            sleep.fields_sleeping,
            sleep.flux_mask_dirty,
        );
        ints!(l0.last_realized_macro_step);
    }

    len!(world.l1_chunks);
    for l1 in &world.l1_chunks {
        h.update(&[l1.axis_len]);
        len!(l1.ph_milli);
        for &ph in &l1.ph_milli {
            ints!(ph);
        }
        len!(l1.fertility);
        for &v in &l1.fertility {
            h.update(&v.to_le_bytes());
        }
        len!(l1.salinity);
        for &v in &l1.salinity {
            h.update(&v.to_le_bytes());
        }
        len!(l1.temperature_dk);
        for &v in &l1.temperature_dk {
            h.update(&v.to_le_bytes());
        }
        len!(l1.microbe_saprotroph);
        h.update(&l1.microbe_saprotroph);
        len!(l1.microbe_conductive);
        h.update(&l1.microbe_conductive);
        len!(l1.microbe_halophile);
        h.update(&l1.microbe_halophile);
        len!(l1.microbe_ferro);
        h.update(&l1.microbe_ferro);
        len!(l1.electric_potential);
        for &v in &l1.electric_potential {
            floats!(v);
        }
        len!(l1.conductivity);
        for &v in &l1.conductivity {
            h.update(&v.to_le_bytes());
        }
        len!(l1.flux_block_mask);
        h.update(&l1.flux_block_mask);
    }

    len!(world.regions);
    for r in &world.regions {
        ints!(r.id.0);
        len!(r.name);
        h.update(r.name.as_bytes());
        bytes!(r.role, r.era, r.weather);
        ints!(
            r.x_start as u64,
            r.x_end as u64,
            r.mean_surface_y,
            r.water_vapor_ul,
            r.pending_surface_water_ul,
            r.precipitation_remainder_ul,
            r.evaporation_remainder_ul,
            r.mineral_dust_units,
            r.surface_charge_uc,
            r.era_elapsed_macro_steps,
            r.era_transitions_count,
            r.flux_history.max_len as u64,
            r.flux_history.discarded_through,
        );
        floats!(
            r.moisture_index,
            r.fertility_index,
            r.salt_crust_ratio,
            r.charge_index,
            r.dust_saturation,
            r.flower_sea_suitability,
        );
        len!(r.flux_history.archived_buckets);
        for bucket in &r.flux_history.archived_buckets {
            ints!(
                bucket.start_step,
                bucket.end_step,
                bucket.water_per_col,
                bucket.settled_water_ul
            );
            bytes!(bucket.era);
            floats!(bucket.flood_peak);
            for value in [
                bucket.acid,
                bucket.salt,
                bucket.flood_exposure,
                bucket.ph,
                bucket.fertility,
                bucket.salinity,
                bucket.charge,
            ] {
                ints!(canonical_f64(value));
            }
        }
        len!(r.flux_history.records);
        for record in &r.flux_history.records {
            ints!(
                record.macro_step,
                record.net_water_delta_per_col,
                record.settled_water_ul
            );
            bytes!(record.era, record.weather);
            floats!(
                record.acid_erosion_impulse,
                record.salt_deposition_impulse,
                record.flood_peak,
                record.ph_delta,
                record.fertility_delta,
                record.salinity_delta,
                record.ambient_charge_v,
            );
        }
    }

    len!(world.surface_reference_height);
    for &height in &world.surface_reference_height {
        ints!(height);
    }

    len!(world.surface_salt_inventory);
    for &salt in &world.surface_salt_inventory {
        ints!(canonical_f64(salt));
    }

    let topo = &world.topology;
    len!(topo.surface_height);
    for &sy in &topo.surface_height {
        ints!(sy);
    }
    len!(topo.aquifer_y);
    for &aq in &topo.aquifer_y {
        ints!(aq);
    }
    len!(topo.travertine_zones);
    for &(start, end) in &topo.travertine_zones {
        ints!(start as u64, end as u64);
    }
    len!(topo.flood_wadis);
    for &x in &topo.flood_wadis {
        ints!(x);
    }
    len!(topo.sinkholes);
    for sh in &topo.sinkholes {
        ints!(
            sh.region_id.0,
            sh.x_center,
            sh.top_y,
            sh.bottom_y,
            sh.mean_radius
        );
    }
    len!(topo.dark_river_edges);
    for edge in &topo.dark_river_edges {
        ints!(
            edge.from_region.0,
            edge.to_region.0,
            edge.y_level,
            edge.min_clearance_cells
        );
    }
    len!(topo.fossil_cavities);
    for &(center, radius) in &topo.fossil_cavities {
        ints!(center.x, center.y, radius);
    }
    len!(topo.ruin_anchors);
    for anchor in &topo.ruin_anchors {
        bytes!(anchor.kind);
        ints!(
            anchor.id,
            anchor.region_id.0,
            anchor.min_x,
            anchor.min_y,
            anchor.max_x,
            anchor.max_y,
            anchor.cavity_center.x,
            anchor.cavity_center.y,
            anchor.waste_heat_bonus_dk,
        );
    }
    ints!(
        topo.initial_ruin_solid_cells,
        topo.initial_total_solid_cells
    );
    floats!(topo.natural_flower_sea_column_ratio);

    *h.finalize().as_bytes()
}

fn canonical_f32(value: f32) -> u32 {
    if value == 0.0 {
        0
    } else if value.is_nan() {
        0x7fc0_0000
    } else {
        value.to_bits()
    }
}

fn canonical_f64(value: f64) -> u64 {
    if value == 0.0 {
        0
    } else if value.is_nan() {
        f64::NAN.to_bits()
    } else {
        value.to_bits()
    }
}

pub fn hash_to_hex(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}
