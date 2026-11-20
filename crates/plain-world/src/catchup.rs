use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::{CellCoord, CELL_MAX_FLUID_UNITS, CHUNK_SIZE};

use crate::spatial::{
    DirtyRect, FieldChunkSoA, MacroFluxRecord, RegionalMacroFluxHistory, TerrainChunkSoA,
    WorldState,
};

/// Replay retained regional flux history for one chunk, conserving all realized water transfers.
pub fn catch_up_chunk(world: &mut WorldState, chunk_idx: usize) -> bool {
    let target_step = world.clock.macro_step.0;
    let last_step = world.l0_chunks[chunk_idx].last_realized_macro_step;
    if last_step >= target_step {
        return false;
    }

    let chunks_x = world.config.dimensions.chunks_x as usize;
    let x_origin = chunk_idx % chunks_x * CHUNK_SIZE;
    let y_origin = chunk_idx / chunks_x * CHUNK_SIZE;
    let center_region = world.region_for_x(x_origin + CHUNK_SIZE / 2).0 as usize;
    let steps_per_day = world.config.clock.macro_steps_per_game_day.max(1) as usize;
    let axis = world.config.field_downsample.tiles_per_chunk_axis();
    let is_surface_chunk = world
        .topology
        .surface_height
        .iter()
        .skip(x_origin)
        .take(CHUNK_SIZE)
        .any(|&sy| sy as usize + 24 >= y_origin && sy as usize <= y_origin + CHUNK_SIZE + 16);
    // A chunk may span two basins, so its cursor can advance only through records published
    // for both. Expired records are intentionally outside the retained replay window.
    let mut realized_step = target_step;
    let mut spans = Vec::new();
    for (region_id, region) in world.regions.iter().enumerate() {
        let x_start = region.x_start.max(x_origin);
        let x_end = region.x_end.min(x_origin + CHUNK_SIZE);
        if x_start >= x_end || (region_id != center_region && !is_surface_chunk) {
            continue;
        }
        let Some(published) = published_through(&region.flux_history, last_step, target_step)
        else {
            return false;
        };
        realized_step = realized_step.min(published);
        spans.push((region_id, x_start, x_end));
    }
    if spans.is_empty()
        || spans
            .iter()
            .any(|&(id, _, _)| world.regions[id].flux_history.discarded_through >= realized_step)
    {
        return false;
    }

    let mut terrain_changed = false;
    for (region_id, x_start, x_end) in spans {
        let records = world.regions[region_id].flux_history.slice_since(last_step);
        let records = &records
            [..records.partition_point(|record| record.macro_step <= realized_step)]
            .to_vec();
        for bucket in flux_buckets(records, steps_per_day) {
            if region_id == center_region {
                apply_fields(
                    &mut world.l1_chunks[chunk_idx],
                    axis,
                    is_surface_chunk,
                    &bucket,
                );
            }
            if is_surface_chunk {
                terrain_changed |= apply_surface_wear(
                    world,
                    chunk_idx,
                    x_origin,
                    y_origin,
                    x_start..x_end,
                    &bucket,
                );
                if bucket.water_per_col != 0 {
                    for wx in x_start..x_end {
                        let Some(&sy) = world.topology.surface_height.get(wx) else {
                            continue;
                        };
                        let sy = usize::from(sy);
                        // A fixed owner keeps one replay cursor per column even when the water
                        // surface lies across a vertical chunk seam.
                        if sy / CHUNK_SIZE == y_origin / CHUNK_SIZE {
                            exchange_surface_water(world, region_id, wx, sy, bucket.water_per_col);
                        }
                    }
                }
            }
        }
    }

    if terrain_changed || world.l0_chunks[chunk_idx].sleep.flux_mask_dirty {
        world.recompute_chunk_flux_mask(chunk_idx);
        world.l0_chunks[chunk_idx].dirty_rect = DirtyRect::full();
        world.l0_chunks[chunk_idx].sleep.fluid_sleeping = false;
    }

    world.l0_chunks[chunk_idx].last_realized_macro_step = realized_step;
    world.l0_chunks[chunk_idx].sleep.dirty_for_save = true;
    true
}

fn published_through(
    history: &RegionalMacroFluxHistory,
    last_step: u64,
    target_step: u64,
) -> Option<u64> {
    let start = last_step.max(history.discarded_through);
    let records = history.slice_since(start);
    let records = &records[..records.partition_point(|record| record.macro_step <= target_step)];
    let end = records.last()?.macro_step;
    if end - start == records.len() as u64 {
        return Some(end);
    }

    // The normal append path is contiguous. If publication stalled or left a gap,
    // stop before the first unwritten step.
    records
        .iter()
        .zip(start + 1..=end)
        .take_while(|(record, expected)| record.macro_step == *expected)
        .map(|(record, _)| record.macro_step)
        .last()
}

#[derive(Default)]
struct FluxBucket {
    acid: f32,
    salt_dep: f32,
    max_flood: f32,
    ph_d: f32,
    fert_d: f32,
    sal_d: f32,
    water_per_col: i64,
}

fn flux_buckets(
    mut records: &[MacroFluxRecord],
    steps_per_day: usize,
) -> impl Iterator<Item = FluxBucket> + '_ {
    std::iter::from_fn(move || {
        let first = records.first()?;
        let mut bucket = FluxBucket::default();
        let mut consumed = 0;
        for record in records.iter().take(steps_per_day) {
            let water = i64::from(record.net_water_delta_per_col);
            // Era transitions can share a bucket with the next era's first record.
            // Replay opposite water transfers separately so neither is canceled by batching.
            if record.era != first.era
                || (water != 0
                    && bucket.water_per_col != 0
                    && water.signum() != bucket.water_per_col.signum())
            {
                break;
            }
            bucket.acid += record.acid_erosion_impulse;
            bucket.salt_dep += record.salt_deposition_impulse;
            bucket.max_flood = bucket.max_flood.max(record.flood_peak);
            bucket.ph_d += record.ph_delta;
            bucket.fert_d += record.fertility_delta;
            bucket.sal_d += record.salinity_delta;
            bucket.water_per_col += water;
            consumed += 1;
        }
        records = &records[consumed..];
        Some(bucket)
    })
}

fn apply_fields(l1: &mut FieldChunkSoA, axis: usize, is_surface_chunk: bool, bucket: &FluxBucket) {
    let FluxBucket {
        acid,
        salt_dep,
        ph_d,
        fert_d,
        sal_d,
        ..
    } = *bucket;
    let surface_attenuation = if is_surface_chunk { 1.0 } else { 0.18 };
    for fy in 0..axis {
        for fx in 0..axis {
            let fi = fy * axis + fx;
            let ph = (l1.ph_scaled[fi] as f32 + ph_d * surface_attenuation).clamp(32.0, 118.0);
            let fert =
                (l1.fertility[fi] as f32 + fert_d * surface_attenuation).clamp(200.0, 65_000.0);
            let sal = (l1.salinity[fi] as f32 + sal_d * surface_attenuation).clamp(200.0, 65_000.0);
            l1.ph_scaled[fi] = ph as u8;
            l1.fertility[fi] = fert as u16;
            l1.salinity[fi] = sal as u16;

            // Update dormant/active saprotroph response to arid vs red-rain recovery
            if acid > 0.15 && ph < 78.0 {
                l1.microbe_saprotroph[fi] = l1.microbe_saprotroph[fi].saturating_add(14).min(230);
            } else if salt_dep > 0.25 {
                l1.microbe_saprotroph[fi] =
                    ((l1.microbe_saprotroph[fi] as f32) * 0.72).max(4.0) as u8;
            }
        }
    }
}

fn apply_surface_wear(
    world: &mut WorldState,
    chunk_idx: usize,
    x_origin: usize,
    y_origin: usize,
    columns: std::ops::Range<usize>,
    bucket: &FluxBucket,
) -> bool {
    let FluxBucket {
        acid,
        salt_dep,
        max_flood,
        ..
    } = *bucket;
    if acid <= 0.05 && max_flood <= 0.35 && salt_dep <= 0.35 {
        return false;
    }
    let mut l0_changed = false;
    for wx in columns {
        let Some(&sy) = world.topology.surface_height.get(wx) else {
            continue;
        };
        let sy = usize::from(sy);
        let lx = wx - x_origin;
        // Locate topmost solid cell inside this chunk column near surface
        let mut top_solid_ly: Option<usize> = None;
        for ly in (0..CHUNK_SIZE).rev() {
            let li = ly * CHUNK_SIZE + lx;
            if world.l0_chunks[chunk_idx].solid_material[li] != MaterialId::AIR.0 {
                top_solid_ly = Some(ly);
                break;
            }
        }

        if let Some(tly) = top_solid_ly {
            let cell = CellCoord::new(wx as i32, (y_origin + tly) as i32);
            // Acid rain dissolution + flood shear strictly via `MaterialDef` physical properties (F5)
            if acid > 0.05 || max_flood > 0.35 {
                let flood_stress_kpa = (max_flood * 3_500.0) as u32;
                if world.apply_physical_wear(cell, flood_stress_kpa, acid * 8.0, 0.0) {
                    l0_changed = true;
                }
            }

            // Salt crust deposition during arid evaporation on exposed low-basin soil/clay
            if salt_dep > 0.35 && tly + 1 < CHUNK_SIZE {
                let above_li = (tly + 1) * CHUNK_SIZE + lx;
                let cur_mat =
                    MaterialId(world.l0_chunks[chunk_idx].solid_material[tly * CHUNK_SIZE + lx]);
                let ch = &mut world.l0_chunks[chunk_idx];
                if ch.solid_material[above_li] == MaterialId::AIR.0
                    && ch.fluid_volume[above_li] == 0
                    && matches!(
                        cur_mat,
                        MaterialId::SILT_CLAY | MaterialId::LOOSE_SOIL_DAM | MaterialId::SANDSTONE
                    )
                    && (wx + sy) % 3 == 0
                {
                    ch.set_solid(lx, tly + 1, MaterialId::SALT_CRUST);
                    l0_changed = true;
                }
            }
        }
    }
    l0_changed
}

/// Exchange water only in the open surface column, terminating at its first solid cell.
fn exchange_surface_water(
    world: &mut WorldState,
    region_id: usize,
    x: usize,
    sy: usize,
    water: i64,
) {
    let region = &world.regions[region_id];
    let mut remaining = if water > 0 {
        water.min(region.pending_surface_water_ul - region.precipitation_remainder_ul)
    } else {
        -water
    };
    if remaining == 0 {
        return;
    }
    let width = world.config.dimensions.chunks_x as usize;
    let cx = x / CHUNK_SIZE;
    let lx = x % CHUNK_SIZE;
    let index = |y: usize| {
        (
            y / CHUNK_SIZE * width + cx,
            y % CHUNK_SIZE * CHUNK_SIZE + lx,
        )
    };
    let lo = sy.saturating_sub(24);
    let hi = (sy + 24).min(world.height_cells() - 1);
    let mut floor = None;
    for y in (lo..=hi).rev() {
        let (ci, li) = index(y);
        let chunk = &mut world.l0_chunks[ci];
        if chunk.solid_material[li] != MaterialId::AIR.0 {
            floor = Some(y);
            break;
        }
        if water < 0 && chunk.fluid_volume[li] != 0 {
            let actual = remaining.min(i64::from(chunk.fluid_volume[li]));
            chunk.fluid_volume[li] -= actual as u16;
            if chunk.fluid_volume[li] == 0 {
                chunk.fluid_kind[li] = FluidKind::None as u8;
            }
            mark_fluid_change(chunk, lx, y % CHUNK_SIZE);
            world.regions[region_id].water_vapor_ul += actual;
            remaining -= actual;
            if remaining == 0 {
                return;
            }
        }
    }

    if let Some(floor) = floor.filter(|_| water > 0) {
        for y in floor + 1..=hi {
            let (ci, li) = index(y);
            let chunk = &mut world.l0_chunks[ci];
            let actual = remaining.min(i64::from(
                CELL_MAX_FLUID_UNITS.saturating_sub(chunk.fluid_volume[li]),
            ));
            if actual != 0 {
                chunk.fluid_volume[li] += actual as u16;
                if chunk.fluid_kind[li] == FluidKind::None as u8 {
                    chunk.fluid_kind[li] = FluidKind::RainPool as u8;
                }
                mark_fluid_change(chunk, lx, y % CHUNK_SIZE);
                world.regions[region_id].pending_surface_water_ul -= actual;
                remaining -= actual;
                if remaining == 0 {
                    return;
                }
            }
        }
    }
}

fn mark_fluid_change(chunk: &mut TerrainChunkSoA, lx: usize, ly: usize) {
    chunk.dirty_rect.mark(lx, ly);
    chunk.sleep.fluid_sleeping = false;
    chunk.sleep.dirty_for_save = true;
}

/// Catch up all chunks inside `active_mask`, respecting an optional max-chunks-per-frame budget
/// to prevent frame-time spikes on observer teleportation (E.3).
pub fn catch_up_active_chunks(
    world: &mut WorldState,
    active_mask: &[bool],
    max_chunks_budget: usize,
) -> usize {
    let mut caught_up = 0usize;
    let target_step = world.clock.macro_step.0;
    for (ci, &active) in active_mask.iter().enumerate() {
        if !active {
            continue;
        }
        if world.l0_chunks[ci].last_realized_macro_step < target_step {
            if catch_up_chunk(world, ci) {
                caught_up += 1;
                if caught_up >= max_chunks_budget {
                    break;
                }
            }
        }
    }
    caught_up
}
