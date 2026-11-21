use plain_contracts::materials::MaterialId;
use plain_contracts::telemetry::EraPhase;
use plain_contracts::{CellCoord, CHUNK_SIZE};

use crate::spatial::{BasinRole, WorldState};

use super::buckets::FluxBucket;

/// Replay surface wear and deposition. The `y <= 8` guard protects the world-grid
/// bottom, deep below the dark river.
pub(super) fn realize_surface_geology(
    world: &mut WorldState,
    region: usize,
    x: usize,
    role: BasinRole,
    bucket: &FluxBucket,
    steps_per_day: u64,
) {
    let reference = reference_height(world, x);
    let y = usize::from(world.topology.surface_height[x]);
    if !world
        .get_material(CellCoord::new(x as i32, y as i32))
        .is_solid()
    {
        return;
    }

    if bucket.acid > 0.0 || bucket.flood_exposure > 0.0 {
        let elapsed = bucket.end - bucket.start + 1;
        let pressure = (bucket.flood_exposure * 2_600.0 / elapsed as f64) as u32;
        let mut start = bucket.start;
        // Expose each day's newly uncovered material to the rest of the storm.
        // Archive runs retain cumulative acid and peak pressure per era.
        while start <= bucket.end {
            let end = start
                .div_ceil(steps_per_day)
                .saturating_mul(steps_per_day)
                .min(bucket.end);
            let portion = (end - start + 1) as f64 / elapsed as f64;
            let y = usize::from(world.topology.surface_height[x]);
            if y <= 8 {
                break;
            }
            let pressure = if end % steps_per_day == 0 {
                pressure
            } else {
                0
            };
            world.apply_physical_wear(
                CellCoord::new(x as i32, y as i32),
                pressure,
                (bucket.acid * portion * 8.0) as f32,
                0.0,
            );
            if end == bucket.end {
                break;
            }
            start = end + 1;
        }
        world.surface_salt_inventory[x] = (world.surface_salt_inventory[x] - bucket.acid).max(0.0);
    }

    let y = usize::from(world.topology.surface_height[x]);
    let above = CellCoord::new(x as i32, y as i32 + 1);
    let substrate = world.get_material(CellCoord::new(x as i32, y as i32));
    let lowland = y <= usize::from(world.regions[region].mean_surface_y) + 8;
    if lowland || role == BasinRole::ElectrolyteSaltBasin {
        // The shallow mineral supply concentrates during evaporation and
        // declines with storm washout and reverse-rain dust uplift.
        const SALT_SATURATION: f64 = 0.12;
        world.surface_salt_inventory[x] =
            (world.surface_salt_inventory[x] + bucket.salt).clamp(0.0, SALT_SATURATION);
        if bucket.era == EraPhase::SaltCrustDesert as u8
            && world.surface_salt_inventory[x] >= SALT_SATURATION
            && matches!(
                substrate,
                MaterialId::SILT_CLAY
                    | MaterialId::LOOSE_SOIL_DAM
                    | MaterialId::SANDSTONE
                    | MaterialId::TRAVERTINE
            )
            && deposit_at_surface(world, above, MaterialId::SALT_CRUST)
        {
            world.surface_salt_inventory[x] -= SALT_SATURATION;
        }
    }

    if bucket.era == EraPhase::FlowerSeaBloom as u8 {
        // Restoring weathered soil and calcifying terraces require exposed
        // accommodation space and enough time for the deposited layer to set.
        let deposit = if role == BasinRole::CanyonTravertineTerrace
            && matches!(substrate, MaterialId::TRAVERTINE | MaterialId::SILT_CLAY)
            && y <= reference
            && (lowland || y < reference)
        {
            Some(MaterialId::TRAVERTINE)
        } else if y < reference
            && matches!(
                substrate,
                MaterialId::SILT_CLAY | MaterialId::LOOSE_SOIL_DAM | MaterialId::SANDSTONE
            )
        {
            Some(MaterialId::SILT_CLAY)
        } else {
            None
        };
        if let Some(material) = deposit {
            if crosses_day(bucket, steps_per_day, 97, (x as u64 * 31) % 97) {
                deposit_at_surface(world, above, material);
            }
        }
    }
}

fn deposit_at_surface(world: &mut WorldState, cell: CellCoord, material: MaterialId) -> bool {
    if !world.config.dimensions.contains_cell(cell)
        || !world.get_material(cell).is_air()
        || world.get_fluid(cell).1 != 0
    {
        return false;
    }
    let x = cell.x as usize;
    let y = cell.y as usize;
    world.set_solid_unchecked_gen(cell, material);
    world.wake_neighbors_if_border(cell.chunk_coord(), x % CHUNK_SIZE, y % CHUNK_SIZE);
    world.topology.surface_height[x] = y as u16;
    true
}

pub(super) fn reference_height(world: &WorldState, x: usize) -> usize {
    world
        .surface_reference_height
        .get(x)
        .or_else(|| world.topology.surface_height.get(x))
        .copied()
        .map_or(usize::MAX, usize::from)
}

fn crosses_day(bucket: &FluxBucket, steps_per_day: u64, period: u64, phase: u64) -> bool {
    let offset = phase * steps_per_day;
    let span = period * steps_per_day;
    (bucket.end + offset) / span > (bucket.start - 1 + offset) / span
}
