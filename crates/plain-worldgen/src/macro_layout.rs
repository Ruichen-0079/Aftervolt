use plain_contracts::telemetry::{EraPhase, WeatherSpectacle};
use plain_contracts::RegionId;
use plain_world::{BasinRole, MacroRegion, RegionalMacroFluxHistory, WorldState};
use rand_core::RngCore;

use crate::noise::fbm1d;

/// Partition the finite 2D world horizontally into staggered, geologically coherent L3 basins.
/// Guarantees every valid seed contains:
/// - At least one high RidgePlateau (catchment & lightning highland),
/// - At least one CanyonTravertineTerrace (stepped waterfalls & wadis),
/// - At least one expansive lowland ElectrolyteSaltBasin (natural Electrolyte Flower Sea).
pub fn build_macro_layout(world: &mut WorldState) {
    let w = world.width_cells();
    let h = world.height_cells();
    let basin_count = usize::from(world.config.basin_count.clamp(3, 8)).min(w);
    if basin_count == 0 {
        world.regions.clear();
        world.topology.aquifer_y.clear();
        world.topology.surface_height.clear();
        return;
    }
    let min_span = (w / basin_count).min(128);
    let mut rng = world.seed.derive_rng("macro-layout", 0);

    // Deterministic permutation of guaranteed core roles + varied secondary basins
    let mut roles = vec![
        BasinRole::RidgePlateau,
        BasinRole::CanyonTravertineTerrace,
        BasinRole::ElectrolyteSaltBasin,
    ];
    roles.truncate(basin_count);
    while roles.len() < basin_count {
        let extra = match (rng.next_u32() % 3) as usize {
            0 => BasinRole::FaultBasin,
            1 => BasinRole::ElectrolyteSaltBasin,
            _ => BasinRole::RidgePlateau,
        };
        roles.push(extra);
    }

    // Fisher-Yates shuffle using deterministic ChaCha20Rng
    for i in (1..roles.len()).rev() {
        let j = (rng.next_u32() as usize) % (i + 1);
        roles.swap(i, j);
    }

    // Assign non-uniform basin widths (ElectrolyteSaltBasin gets slightly wider floor for flower sea)
    let mut weights: Vec<f64> = roles
        .iter()
        .map(|r| {
            let jitter = 0.88 + ((rng.next_u32() % 250) as f64) * 0.001;
            match r {
                BasinRole::ElectrolyteSaltBasin => 1.28 * jitter,
                BasinRole::RidgePlateau => 0.92 * jitter,
                BasinRole::CanyonTravertineTerrace => 1.02 * jitter,
                BasinRole::FaultBasin => 0.96 * jitter,
            }
        })
        .collect();
    let w_sum: f64 = weights.iter().sum();
    for wt in &mut weights {
        *wt /= w_sum;
    }

    let history_cap = (world.config.four_era_cycle.target_cycle_days as usize)
        .saturating_mul(world.config.clock.macro_steps_per_game_day.max(1) as usize)
        .saturating_mul(2);
    let history_cap = history_cap.clamp(4096, 65_536);

    let mut regions = Vec::with_capacity(basin_count);
    let mut cursor = 0usize;

    // Stagger starting 4-era phases across basins so world begins asynchronously alive
    let starting_eras = [
        EraPhase::FlowerSeaBloom,
        EraPhase::SaltCrustDesert,
        EraPhase::RustRedRainFlood,
        EraPhase::SilentReverseRain,
    ];

    for (i, &role) in roles.iter().enumerate() {
        let remaining = w - cursor;
        let basins_left = basin_count - i - 1;
        let span = if basins_left == 0 {
            remaining
        } else {
            let max_span = remaining - min_span * basins_left;
            ((weights[i] * w as f64) as usize).clamp(min_span, max_span)
        };
        let x_start = cursor;
        let x_end = cursor + span;
        cursor = x_end;

        let mean_y_frac = match role {
            BasinRole::RidgePlateau => 0.74,
            BasinRole::CanyonTravertineTerrace => 0.58,
            BasinRole::FaultBasin => 0.54,
            BasinRole::ElectrolyteSaltBasin => 0.44,
        };
        let mean_surface_y = ((h as f64) * mean_y_frac) as u16;

        let era = starting_eras[i % starting_eras.len()];
        let (moisture, fert, salt, charge, dust, weather) = match era {
            EraPhase::FlowerSeaBloom => (
                0.80,
                0.82,
                0.14,
                0.18,
                0.12,
                WeatherSpectacle::WanderingBallLightning,
            ),
            EraPhase::SaltCrustDesert => (0.14, 0.24, 0.78, 0.56, 0.42, WeatherSpectacle::Calm),
            EraPhase::SilentReverseRain => (
                0.10,
                0.18,
                0.82,
                0.84,
                0.62,
                WeatherSpectacle::SilentReverseRain,
            ),
            EraPhase::RustRedRainFlood => {
                (0.48, 0.56, 0.46, 0.32, 0.76, WeatherSpectacle::RustRedRain)
            }
        };

        let role_label = match role {
            BasinRole::RidgePlateau => "断脊高原",
            BasinRole::CanyonTravertineTerrace => "钙华峡谷",
            BasinRole::ElectrolyteSaltBasin => "电解盐盆",
            BasinRole::FaultBasin => "断层荒谷",
        };

        regions.push(MacroRegion {
            id: RegionId(i as u16),
            name: format!("流域{}·{}", i + 1, role_label),
            role,
            x_start,
            x_end,
            mean_surface_y,
            water_vapor_ul: 0, // Populated during hydrology_init for exact global conservation
            pending_surface_water_ul: 0,
            precipitation_remainder_ul: 0,
            evaporation_remainder_ul: 0,
            mineral_dust_units: (dust * 200_000.0) as i64,
            surface_charge_uc: (charge * 1_000_000.0) as i64,
            moisture_index: moisture,
            fertility_index: fert,
            salt_crust_ratio: salt,
            charge_index: charge,
            dust_saturation: dust,
            flower_sea_suitability: if role == BasinRole::ElectrolyteSaltBasin {
                0.85
            } else {
                0.35
            },
            era,
            weather,
            era_elapsed_macro_steps: 0,
            era_transitions_count: 0,
            flux_history: RegionalMacroFluxHistory::new(history_cap),
        });
    }

    // Build smooth continuous deep aquifer ceiling profile (11%..17% of world height)
    let aq_seed = world.seed.derive_u64("aquifer-profile", 0);
    let mut aquifer_y = vec![0u16; w];
    for (x, slot) in aquifer_y.iter_mut().enumerate() {
        let n = fbm1d(aq_seed, x as f64 * 0.0018, 3);
        let base = (h as f64) * 0.145 + n * (h as f64) * 0.028;
        *slot = (base.clamp((h as f64) * 0.09, (h as f64) * 0.21)) as u16;
    }

    world.regions = regions;
    world.topology.aquifer_y = aquifer_y;
    world.topology.surface_height = vec![0u16; w];
}
