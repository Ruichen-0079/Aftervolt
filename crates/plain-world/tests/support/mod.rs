use plain_contracts::{MasterSeed, WorldConfig, WorldDimensions};
use plain_world::WorldState;

#[allow(dead_code)]
pub mod three_modes;

pub fn flat_test_world() -> WorldState {
    let mut cfg = WorldConfig::preset_ci_1024x512();
    cfg.dimensions = WorldDimensions::from_chunks(2, 2);
    cfg.active_lod0_padding_chunks = 0;
    cfg.four_era_cycle.cross_basin_wind_transfer_per_day = 0.0;
    let mut world = WorldState::new_empty(cfg, MasterSeed::from_u64(12), 1);
    world.topology.surface_height = vec![63; world.width_cells()];
    world.regions.push(plain_world::MacroRegion {
        id: plain_contracts::RegionId(0),
        name: "Test".into(),
        role: plain_world::BasinRole::ElectrolyteSaltBasin,
        x_start: 0,
        x_end: world.width_cells(),
        mean_surface_y: 256,
        water_vapor_ul: 0,
        pending_surface_water_ul: 0,
        precipitation_remainder_ul: 0,
        evaporation_remainder_ul: 0,
        mineral_dust_units: 0,
        surface_charge_uc: 0,
        moisture_index: 0.5,
        fertility_index: 0.5,
        salt_crust_ratio: 0.2,
        charge_index: 0.2,
        dust_saturation: 0.2,
        flower_sea_suitability: 0.5,
        era: plain_contracts::EraPhase::FlowerSeaBloom,
        weather: plain_contracts::WeatherSpectacle::Calm,
        era_elapsed_macro_steps: 0,
        era_transitions_count: 0,
        flux_history: plain_world::RegionalMacroFluxHistory::new(256),
    });

    world
}
