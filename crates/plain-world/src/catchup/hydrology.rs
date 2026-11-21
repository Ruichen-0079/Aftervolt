use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::{CELL_MAX_FLUID_UNITS, CHUNK_SIZE};

use crate::spatial::{TerrainChunkSoA, WorldState};

/// Settle daily atmosphere/surface exchange for every basin, including sleeping chunks.
/// This balances real reservoirs at a fixed game-time boundary regardless of activation.
pub(crate) fn settle_basin_water(world: &mut WorldState, region_id: usize, day: u64) -> i64 {
    let region = &mut world.regions[region_id];
    let columns = region.x_start..region.x_end;
    let precipitation = region.precipitation_remainder_ul;
    let demand = std::mem::take(&mut region.evaporation_remainder_ul);
    if precipitation > 0 && !columns.is_empty() {
        let span = columns.len() as i64;
        let mut realized = 0;
        for (offset, x) in columns.clone().enumerate() {
            let share = precipitation / span + i64::from((offset as i64) < precipitation % span);
            if let Some(&sy) = world.topology.surface_height.get(x) {
                realized += precipitate_column(world, x, usize::from(sy), share);
            }
        }
        let region = &mut world.regions[region_id];
        region.pending_surface_water_ul -= realized;
        region.precipitation_remainder_ul -= realized;
    }
    if demand > 0 {
        let region = &mut world.regions[region_id];
        let pending = demand.min(region.pending_surface_water_ul);
        region.pending_surface_water_ul -= pending;
        region.precipitation_remainder_ul = (region.precipitation_remainder_ul - pending).max(0);
        let mut remaining = demand - pending;
        // Pool unmet demand across the basin so a dry column does not waste it.
        if !columns.is_empty() && remaining > 0 {
            let span = columns.len();
            let start = (day % span as u64) as usize;
            for k in 0..span {
                let x = columns.start + (start + k) % span;
                if let Some(&sy) = world.topology.surface_height.get(x) {
                    remaining -= evaporate_column(world, x, usize::from(sy), remaining);
                    if remaining == 0 {
                        break;
                    }
                }
            }
        }
        if remaining > 0 {
            remaining -= transpire_aquifer(world, region_id, remaining, day);
        }
        let evaporated = demand - remaining;
        world.regions[region_id].water_vapor_ul += evaporated;
        return evaporated;
    }
    0
}

fn column_index(world: &WorldState, x: usize, y: usize) -> (usize, usize) {
    let width = world.config.dimensions.chunks_x as usize;
    (
        y / CHUNK_SIZE * width + x / CHUNK_SIZE,
        y % CHUNK_SIZE * CHUNK_SIZE + x % CHUNK_SIZE,
    )
}

pub(super) fn precipitate_column(
    world: &mut WorldState,
    x: usize,
    sy: usize,
    mut remaining: i64,
) -> i64 {
    let requested = remaining;
    if remaining <= 0 {
        return 0;
    }
    let hi = (sy + 24).min(world.height_cells() - 1);
    let floor = (sy.saturating_sub(24)..=hi).rev().find(|&y| {
        let (ci, li) = column_index(world, x, y);
        world.l0_chunks[ci].solid_material[li] != MaterialId::AIR.0
    });
    let Some(floor) = floor else { return 0 };
    for y in floor + 1..=hi {
        let (ci, li) = column_index(world, x, y);
        let chunk = &mut world.l0_chunks[ci];
        let actual = remaining.min(i64::from(
            CELL_MAX_FLUID_UNITS.saturating_sub(chunk.fluid_volume[li]),
        ));
        if actual != 0 {
            chunk.fluid_volume[li] += actual as u16;
            if chunk.fluid_kind[li] == FluidKind::None as u8 {
                chunk.fluid_kind[li] = FluidKind::RainPool as u8;
            }
            mark_fluid_change(chunk, x, y);
            remaining -= actual;
            if remaining == 0 {
                break;
            }
        }
    }
    requested - remaining
}

pub(super) fn evaporate_column(
    world: &mut WorldState,
    x: usize,
    sy: usize,
    mut remaining: i64,
) -> i64 {
    let requested = remaining;
    let hi = (sy + 24).min(world.height_cells() - 1);
    for y in (sy.saturating_sub(24)..=hi).rev() {
        let (ci, li) = column_index(world, x, y);
        let chunk = &mut world.l0_chunks[ci];
        if chunk.solid_material[li] != MaterialId::AIR.0 {
            break;
        }
        remaining -= remove_water(
            chunk,
            li,
            x,
            y,
            remaining,
            aquifer_reserve(chunk.fluid_kind[li]),
        );
        if remaining == 0 {
            break;
        }
    }
    requested - remaining
}

pub(super) fn transpire_aquifer(
    world: &mut WorldState,
    region_id: usize,
    mut remaining: i64,
    day: u64,
) -> i64 {
    let requested = remaining;
    let region = &world.regions[region_id];
    let span = region.x_end - region.x_start;
    if span == 0 {
        return 0;
    }
    let start = region.x_start;
    world
        .groundwater
        .refresh(&world.l0_chunks, world.config.dimensions.chunks_x as usize);
    let rotation = (day % span as u64) as usize;
    // The mapped aquifer usually supplies the whole budget. Search perched
    // groundwater below the evaporation band only if the river is depleted.
    for in_aquifer in [true, false] {
        for k in 0..span {
            let x = start + (rotation + k) % span;
            let Some(&surface) = world.topology.surface_height.get(x) else {
                continue;
            };
            let Some(ceiling) = usize::from(surface).checked_sub(25) else {
                continue;
            };
            let aquifer = world
                .topology
                .aquifer_y
                .get(x)
                .map_or(0, |&y| usize::from(y) + 8)
                .min(ceiling);
            let (bottom, top) = if in_aquifer {
                (0, aquifer)
            } else {
                (aquifer + 1, ceiling)
            };
            for y in (bottom..=top).rev() {
                let (ci, li) = column_index(world, x, y);
                if world.l0_chunks[ci].fluid_volume[li] == 0 || !world.groundwater.contains(ci, li)
                {
                    continue;
                }
                let chunk = &mut world.l0_chunks[ci];
                remaining -= remove_water(
                    chunk,
                    li,
                    x,
                    y,
                    remaining,
                    aquifer_reserve(chunk.fluid_kind[li]),
                );
                if remaining == 0 {
                    return requested;
                }
            }
        }
    }
    requested - remaining
}

fn aquifer_reserve(kind: u8) -> u16 {
    if kind == FluidKind::BlackWater as u8 {
        1_500
    } else {
        0
    }
}

fn remove_water(
    chunk: &mut TerrainChunkSoA,
    index: usize,
    x: usize,
    y: usize,
    demand: i64,
    reserve: u16,
) -> i64 {
    let volume = &mut chunk.fluid_volume[index];
    let actual = demand.min(i64::from(volume.saturating_sub(reserve)));
    if actual > 0 {
        *volume -= actual as u16;
        if *volume == 0 {
            chunk.fluid_kind[index] = FluidKind::None as u8;
        }
        mark_fluid_change(chunk, x, y);
    }
    actual
}

fn mark_fluid_change(chunk: &mut TerrainChunkSoA, x: usize, y: usize) {
    chunk.tile_conditions.clear();
    chunk.dirty_rect.mark(x % CHUNK_SIZE, y % CHUNK_SIZE);
    chunk.sleep.fluid_sleeping = false;
    chunk.sleep.dirty_for_save = true;
}
