use plain_contracts::rng::det_pow;
use plain_contracts::telemetry::{EraPhase, WeatherSpectacle};

use crate::spatial::{MacroFluxRecord, WorldState};

/// Advance all L3 MacroRegions by 1 MacroStep (`dt_days = 1 / macro_steps_per_game_day`).
///
/// Enforces:
/// 1. Default 1220-game-day non-uniform, non-linear 4-era hysteresis cycle (WorldConfig configurable).
/// 2. Cross-basin wind transport of atmospheric water vapor and mineral dust (spatial asynchrony).
/// 3. Exact `i64` integer conservation of total world water across surface/pending/vapor reservoirs.
/// 4. Appends compact `MacroFluxRecord` to each region for dormant chunk lazy catch-up (E.4 & Q7).
///
/// Returns `true` if any basin transitioned to a new `EraPhase` on this step.
pub fn step_macro_climate(world: &mut WorldState) -> bool {
    let n_regions = world.regions.len();
    if n_regions == 0 {
        return false;
    }

    let steps_per_day = world.config.clock.macro_steps_per_game_day.max(1) as f32;
    let dt_days = 1.0 / steps_per_day;
    let cfg = world.config.four_era_cycle;
    let cycle_days = (cfg.target_cycle_days.max(40)) as f32;
    let w_sum =
        (cfg.bloom_weight + cfg.desert_weight + cfg.reverse_rain_weight + cfg.red_rain_weight)
            .max(0.01);

    // Calibrated so the average nonlinear self-sustaining full 4-era cycle matches `target_cycle_days` (1220d).
    let cal = 1.38f32;
    let d_bloom = (cycle_days * (cfg.bloom_weight / w_sum) * cal).max(5.0);
    let d_desert = (cycle_days * (cfg.desert_weight / w_sum) * cal).max(5.0);
    let d_rev = (cycle_days * (cfg.reverse_rain_weight / w_sum) * cal).max(2.0);
    let d_red = (cycle_days * (cfg.red_rain_weight / w_sum) * cal).max(2.0);
    let exp = cfg.nonlinearity_exponent.clamp(1.2, 5.0) as f64;
    let hyst = cfg.hysteresis_margin.clamp(0.04, 0.30);
    let current_step = world.clock.macro_step.0;

    // -------------------------------------------------------------------------
    // 1. Conserved cross-basin high-altitude wind transport (East -> West wrap)
    //    Drives spatial asynchrony: arid evaporating basins feed neighboring rainstorms.
    // -------------------------------------------------------------------------
    let wind_frac = (cfg.cross_basin_wind_transfer_per_day * dt_days).clamp(0.0, 0.25) as f64;
    if n_regions > 1 && wind_frac > 0.0 {
        let mut vapor_out = vec![0i64; n_regions];
        let mut dust_out = vec![0i64; n_regions];
        let mut dust_sat_out = vec![0.0f32; n_regions];

        for (i, r) in world.regions.iter().enumerate() {
            // Basins in ReverseRain / Desert export extra uplifted dust; Bloom exports moist vapor
            let dust_boost = match r.era {
                EraPhase::SilentReverseRain => 2.2,
                EraPhase::SaltCrustDesert => 1.3,
                _ => 0.7,
            };
            vapor_out[i] = ((r.water_vapor_ul.max(0) as f64) * wind_frac) as i64;
            dust_out[i] = ((r.mineral_dust_units.max(0) as f64) * wind_frac * dust_boost) as i64;
            dust_sat_out[i] = r.dust_saturation * (wind_frac as f32) * (dust_boost as f32) * 0.35;
        }

        for i in 0..n_regions {
            // Wind blows from basin i to neighbor (i + n - 1) % n (east -> west)
            let dst = (i + n_regions - 1) % n_regions;
            let dv = vapor_out[i];
            let dd = dust_out[i];
            let ds = dust_sat_out[i];

            world.regions[i].water_vapor_ul -= dv;
            world.regions[dst].water_vapor_ul += dv;

            world.regions[i].mineral_dust_units -= dd;
            world.regions[dst].mineral_dust_units += dd;

            world.regions[i].dust_saturation =
                (world.regions[i].dust_saturation - ds).clamp(0.0, 1.0);
            world.regions[dst].dust_saturation =
                (world.regions[dst].dust_saturation + ds).clamp(0.0, 1.0);
        }
    }

    // -------------------------------------------------------------------------
    // 2. Per-region non-uniform, non-linear 4-era hysteresis evolution
    // -------------------------------------------------------------------------
    let mut any_transition = false;
    let day_hour_norm = ((current_step % (steps_per_day as u64)) as f32) / steps_per_day;
    let is_night = !(0.22..=0.78).contains(&day_hour_norm);

    for r in &mut world.regions {
        r.era_elapsed_macro_steps += 1;
        let span_cols = (r.x_end.saturating_sub(r.x_start)).max(1) as i64;
        let prev_era = r.era;

        let mut acid_impulse = 0.0f32;
        let mut salt_dep_impulse = 0.0f32;
        let mut flood_peak = 0.0f32;
        let mut ph_delta = 0.0f32;
        let mut fert_delta = 0.0f32;
        let mut sal_delta = 0.0f32;
        let ambient_charge_v: f32;
        let mut net_water_per_col = 0;

        match r.era {
            // =================================================================
            // ERA 4: FlowerSeaBloom (Long prosperity -> nonlinear water/fertility drawdown)
            // =================================================================
            EraPhase::FlowerSeaBloom => {
                // Non-linear drawdown accelerates late in bloom as peak canopy density peaks
                let progress = (1.0 - r.moisture_index).clamp(0.0, 1.0) as f64;
                let accel = 0.65 + 0.90 * (det_pow(progress, exp * 0.5) as f32);
                let step_rate = (dt_days / d_bloom) * accel;

                r.moisture_index = (r.moisture_index - step_rate * 0.85).clamp(0.0, 1.0);
                r.fertility_index = (r.fertility_index - step_rate * 0.72).clamp(0.05, 1.0);
                // Evapotranspiration slowly draws subsurface salts toward the surface
                r.salt_crust_ratio = (r.salt_crust_ratio + step_rate * 0.55).clamp(0.0, 1.0);
                r.charge_index = (r.charge_index * 0.996 + 0.001).clamp(0.05, 0.45);
                r.dust_saturation = (r.dust_saturation * 0.995).clamp(0.0, 1.0);

                // Conserved evaporation from surface/pending water into basin atmosphere
                let evap = ((span_cols * 35) as f64 * step_rate as f64 * 60.0) as i64;
                net_water_per_col = evaporate_pending(r, evap, span_cols);

                // Near-ground conductive ion mist from dense flower canopy drives BallLightning & Aurora nights
                r.weather = if is_night && r.moisture_index > 0.45 && (current_step % 7 < 3) {
                    WeatherSpectacle::AuroraMagStorm
                } else if r.moisture_index > 0.35 && (current_step % 5 < 2) {
                    WeatherSpectacle::WanderingBallLightning
                } else {
                    WeatherSpectacle::Calm
                };

                salt_dep_impulse = step_rate * 0.35;
                fert_delta = -step_rate * 1200.0;
                sal_delta = step_rate * 1600.0;
                ph_delta = step_rate * 1.4;
                ambient_charge_v = 45.0 + r.moisture_index * 35.0;

                // Hysteresis exit into Era 1 (SaltCrustDesert)
                if r.moisture_index <= (0.20 - hyst * 0.35).max(0.08)
                    && r.salt_crust_ratio >= (0.50 + hyst * 0.25)
                {
                    r.era = EraPhase::SaltCrustDesert;
                }
            }

            // =================================================================
            // ERA 1: SaltCrustDesert (Long arid hardening -> nonlinear charge & dust buildup)
            // =================================================================
            EraPhase::SaltCrustDesert => {
                // Non-linear electrostatic buildup: starts slowly while crust hardens,
                // then surges exponentially once bare salt crust seals the surface.
                let dryness = (1.0 - r.moisture_index).clamp(0.0, 1.0) as f64;
                let crust_boost =
                    0.55 + 1.15 * (det_pow(r.salt_crust_ratio as f64, exp * 0.6) as f32);
                let step_rate = (dt_days / d_desert) * crust_boost;

                r.moisture_index = (r.moisture_index - step_rate * 0.25).clamp(0.03, 1.0);
                r.salt_crust_ratio = (r.salt_crust_ratio + step_rate * 0.65).clamp(0.0, 1.0);
                r.fertility_index = (r.fertility_index - step_rate * 0.15).clamp(0.04, 1.0);
                r.charge_index =
                    (r.charge_index + step_rate * (0.88 + 0.35 * dryness as f32)).clamp(0.0, 1.0);
                r.dust_saturation = (r.dust_saturation + step_rate * 0.32).clamp(0.0, 1.0);

                let evap = ((span_cols * 14) as f64 * step_rate as f64 * 40.0) as i64;
                net_water_per_col = evaporate_pending(r, evap, span_cols);

                r.surface_charge_uc = ((r.charge_index as f64) * 1_000_000.0) as i64;
                r.mineral_dust_units =
                    (r.mineral_dust_units + (step_rate * 4000.0) as i64).min(5_000_000);
                r.weather = WeatherSpectacle::Calm;

                salt_dep_impulse = step_rate * 1.4;
                ph_delta = step_rate * 3.5; // Alkaline crust drift
                sal_delta = step_rate * 2600.0;
                fert_delta = -step_rate * 350.0;
                ambient_charge_v = 35.0 + r.charge_index * 110.0;

                // Hysteresis dielectric breakdown into Era 2 (SilentReverseRain)
                if r.charge_index >= (0.78 + hyst * 0.5).min(0.95) {
                    r.era = EraPhase::SilentReverseRain;
                }
            }

            // =================================================================
            // ERA 2: SilentReverseRain (Short intense upward dust/charge uplift)
            // =================================================================
            EraPhase::SilentReverseRain => {
                // Sharp nonlinear pulse: rapid dust pumping into upper atmosphere while discharging surface
                let pulse = 0.75 + (det_pow(r.charge_index as f64, exp * 0.5) as f32);
                let step_rate = (dt_days / d_rev) * pulse;

                r.dust_saturation = (r.dust_saturation + step_rate * 0.92).clamp(0.0, 1.0);
                r.charge_index = (r.charge_index - step_rate * 0.58).clamp(0.0, 1.0);
                r.mineral_dust_units =
                    (r.mineral_dust_units + (step_rate * 28_000.0) as i64).min(5_000_000);
                r.surface_charge_uc = ((r.charge_index as f64) * 1_000_000.0) as i64;
                r.weather = WeatherSpectacle::SilentReverseRain;

                // Upward electrostatic lift slightly strips loose surface salt powder
                salt_dep_impulse = -step_rate * 0.20;
                ambient_charge_v = 160.0 + r.charge_index * 140.0;

                // Saturation condensation breakdown into Era 3 (RustRedRainFlood)
                if r.dust_saturation >= (0.80 + hyst * 0.4).min(0.94)
                    || r.charge_index <= (0.28 - hyst * 0.25).max(0.12)
                {
                    r.era = EraPhase::RustRedRainFlood;
                }
            }

            // =================================================================
            // ERA 3: RustRedRainFlood (Short violent acid rain, salt dissolution & flood bloom)
            // =================================================================
            EraPhase::RustRedRainFlood => {
                // Sharp nonlinear flood surge peaking early and tapering as upper dust clears
                let storm_intensity =
                    0.65 + 1.25 * (det_pow(r.dust_saturation as f64, exp * 0.5) as f32);
                let step_rate = (dt_days / d_red) * storm_intensity;

                r.moisture_index = (r.moisture_index + step_rate * 1.05).clamp(0.0, 1.0);
                r.fertility_index = (r.fertility_index + step_rate * 1.10).clamp(0.0, 1.0);
                r.salt_crust_ratio = (r.salt_crust_ratio - step_rate * 1.25).clamp(0.0, 1.0);
                r.dust_saturation = (r.dust_saturation - step_rate * 0.95).clamp(0.0, 1.0);
                r.charge_index = (r.charge_index - step_rate * 0.40).clamp(0.05, 1.0);

                // Conserved heavy precipitation from basin atmosphere back to surface/pending water
                let precip_target =
                    ((r.water_vapor_ul.max(0) as f64) * (step_rate as f64 * 0.45)) as i64;
                let precip = precip_target.min(r.water_vapor_ul.max(0));
                r.water_vapor_ul -= precip;
                r.pending_surface_water_ul += precip;
                r.precipitation_remainder_ul += precip;
                net_water_per_col = take_per_column(&mut r.precipitation_remainder_ul, span_cols);

                r.mineral_dust_units =
                    ((r.mineral_dust_units as f32) * (1.0 - step_rate * 0.6).max(0.0)) as i64;
                r.surface_charge_uc = ((r.charge_index as f64) * 1_000_000.0) as i64;
                r.weather = WeatherSpectacle::RustRedRain;

                acid_impulse = step_rate * 2.4 * cfg.red_rain_acid_intensity;
                flood_peak = (storm_intensity * 0.65).clamp(0.1, 1.0);
                ph_delta = -step_rate * 9.5 * cfg.red_rain_acid_intensity;
                fert_delta = step_rate * 4800.0;
                sal_delta = -step_rate * 3800.0;
                ambient_charge_v = 75.0;

                // Hysteresis transition into Era 4 (FlowerSeaBloom)
                if r.moisture_index >= (0.76 + hyst * 0.35).min(0.92)
                    && r.dust_saturation <= (0.22 - hyst * 0.2).max(0.08)
                {
                    r.era = EraPhase::FlowerSeaBloom;
                }
            }
        }

        if r.era != prev_era {
            r.era_elapsed_macro_steps = 0;
            r.era_transitions_count += 1;
            any_transition = true;
        }

        // Update dynamic natural Electrolyte Flower Sea suitability
        r.flower_sea_suitability = (r.moisture_index * 0.45
            + r.fertility_index * 0.35
            + (1.0 - r.salt_crust_ratio) * 0.20)
            .clamp(0.0, 1.0);

        r.flux_history.push(MacroFluxRecord {
            macro_step: current_step,
            era: r.era as u8,
            weather: r.weather as u8,
            acid_erosion_impulse: acid_impulse,
            salt_deposition_impulse: salt_dep_impulse,
            flood_peak,
            ph_delta,
            fertility_delta: fert_delta,
            salinity_delta: sal_delta,
            ambient_charge_v,
            net_water_delta_per_col: net_water_per_col,
        });
    }

    any_transition
}

/// Redistribute whole multiples of the basin width while retaining every unissued microliter.
fn take_per_column(remainder_ul: &mut i64, span_cols: i64) -> i32 {
    let per_col = (*remainder_ul / span_cols).min(i32::MAX as i64);
    *remainder_ul -= per_col * span_cols;
    per_col as i32
}

fn evaporate_pending(region: &mut crate::spatial::MacroRegion, demand: i64, span_cols: i64) -> i32 {
    let from_pending = demand.min(region.pending_surface_water_ul);
    region.pending_surface_water_ul -= from_pending;
    region.water_vapor_ul += from_pending;
    region.precipitation_remainder_ul = region
        .precipitation_remainder_ul
        .saturating_sub(from_pending)
        .max(0);
    region.evaporation_remainder_ul += demand - from_pending;
    -take_per_column(&mut region.evaporation_remainder_ul, span_cols)
}
