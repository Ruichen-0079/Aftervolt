use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::telemetry::EraPhase;
use plain_contracts::{CellCoord, CELL_MAX_FLUID_UNITS, CHUNK_SIZE};

use crate::spatial::{
    ArchivedFluxRecord, BasinRole, FieldChunkSoA, MacroFluxRecord, RegionalMacroFluxHistory,
    TerrainChunkSoA, WorldState,
};

/// Replay contiguous archived and detailed regional flux for one chunk.
pub fn catch_up_chunk(world: &mut WorldState, chunk_idx: usize) -> bool {
    let target_step = world.clock.macro_step.0;
    let last_step = world.l0_chunks[chunk_idx].last_realized_macro_step;
    if last_step >= target_step {
        return false;
    }

    let chunks_x = world.config.dimensions.chunks_x as usize;
    let x_origin = chunk_idx % chunks_x * CHUNK_SIZE;
    let y_origin = chunk_idx / chunks_x * CHUNK_SIZE;
    let steps_per_day = u64::from(world.config.clock.macro_steps_per_game_day.max(1));
    let ds = world.config.field_downsample.factor();

    let mut realized_step = target_step;
    let mut spans = Vec::new();
    for (id, region) in world.regions.iter().enumerate() {
        let start = region.x_start.max(x_origin);
        let end = region.x_end.min(x_origin + CHUNK_SIZE);
        if start < end {
            realized_step = realized_step.min(published_through(
                &region.flux_history,
                last_step,
                target_step,
            ));
            spans.push((id, start..end));
        }
    }
    if spans.is_empty() || realized_step == last_step {
        return false;
    }

    if world.surface_reference_height.is_empty() {
        world
            .surface_reference_height
            .clone_from(&world.topology.surface_height);
    }
    for (id, columns) in spans {
        let history = &world.regions[id].flux_history;
        let mut buckets = Vec::new();
        let archive_start = history
            .archived_buckets
            .partition_point(|b| b.end_step <= last_step);
        for record in &history.archived_buckets[archive_start..] {
            if record.start_step > realized_step {
                break;
            }
            buckets.push(FluxBucket::from_archive(
                record,
                last_step + 1,
                realized_step,
            ));
        }
        for record in history
            .slice_since(last_step)
            .iter()
            .take_while(|r| r.macro_step <= realized_step)
        {
            if buckets.last().is_none_or(|bucket| {
                bucket.archived
                    || bucket.era != record.era
                    || (record.net_water_delta_per_col != 0
                        && bucket.water != 0
                        && i64::from(record.net_water_delta_per_col).signum()
                            != bucket.water.signum())
            }) {
                buckets.push(FluxBucket {
                    start: record.macro_step,
                    end: record.macro_step,
                    era: record.era,
                    ..FluxBucket::default()
                });
            }
            buckets.last_mut().unwrap().add(record);
        }

        let role = world.regions[id].role;
        for bucket in buckets {
            let days = (bucket.end - bucket.start + 1) as f64 / steps_per_day as f64;
            // Assign an L1 tile by its center, even when a basin boundary cuts a chunk.
            let axis = CHUNK_SIZE / ds;
            let shallow = FieldRelaxation::new(role, &bucket, days, 1.0);
            let deep = FieldRelaxation::new(role, &bucket, days, 0.18);
            for fx in 0..axis {
                let x = x_origin + fx * ds + ds / 2;
                if !columns.contains(&x) {
                    continue;
                }
                let surface = reference_height(world, x);
                for fy in 0..axis {
                    let y = y_origin + fy * ds + ds / 2;
                    let relaxation = if y.abs_diff(surface) <= 64 {
                        &shallow
                    } else {
                        &deep
                    };
                    relaxation.apply(&mut world.l1_chunks[chunk_idx], fy * axis + fx);
                }
            }
            for x in columns.clone() {
                if reference_height(world, x) / CHUNK_SIZE != y_origin / CHUNK_SIZE {
                    continue;
                }
                realize_surface_geology(world, id, x, role, &bucket, steps_per_day);
                if bucket.water != 0 {
                    let y = usize::from(world.topology.surface_height[x]);
                    if bucket.water > 0 {
                        let region = &mut world.regions[id];
                        let available =
                            region.pending_surface_water_ul - region.precipitation_remainder_ul;
                        let applied = precipitate_column(world, x, y, bucket.water.min(available));
                        world.regions[id].pending_surface_water_ul -= applied;
                    } else {
                        let demand = -bucket.water;
                        let from_surface = evaporate_column(world, x, y, demand);
                        let deep = if from_surface < demand {
                            transpire_aquifer(world, id, demand - from_surface, bucket.end)
                        } else {
                            0
                        };
                        world.regions[id].water_vapor_ul += from_surface + deep;
                    }
                }
            }
        }
    }

    if world.l0_chunks[chunk_idx].sleep.flux_mask_dirty {
        world.recompute_chunk_flux_mask(chunk_idx);
    }
    world.l0_chunks[chunk_idx].last_realized_macro_step = realized_step;
    world.l0_chunks[chunk_idx].sleep.dirty_for_save = true;
    true
}

fn reference_height(world: &WorldState, x: usize) -> usize {
    world
        .surface_reference_height
        .get(x)
        .or_else(|| world.topology.surface_height.get(x))
        .copied()
        .map_or(usize::MAX, usize::from)
}

fn published_through(history: &RegionalMacroFluxHistory, last: u64, target: u64) -> u64 {
    let mut end = last;
    let start = history
        .archived_buckets
        .partition_point(|bucket| bucket.end_step <= last);
    for archived in &history.archived_buckets[start..] {
        if archived.start_step > end + 1 {
            return end;
        }
        end = archived.end_step.min(target);
        if end == target {
            return end;
        }
    }
    let records = history.slice_since(end);
    let records = &records[..records.partition_point(|r| r.macro_step <= target)];
    if let Some(last) = records.last() {
        if last.macro_step - end == records.len() as u64 {
            return last.macro_step;
        }
    }
    for record in records {
        if record.macro_step != end + 1 {
            break;
        }
        end = record.macro_step;
    }
    end
}

#[derive(Default)]
struct FluxBucket {
    start: u64,
    end: u64,
    era: u8,
    archived: bool,
    acid: f64,
    salt: f64,
    flood_exposure: f64,
    ph: f64,
    fertility: f64,
    salinity: f64,
    water: i64,
}

impl FluxBucket {
    fn add(&mut self, record: &MacroFluxRecord) {
        self.end = record.macro_step;
        self.acid += f64::from(record.acid_erosion_impulse);
        self.salt += f64::from(record.salt_deposition_impulse);
        self.flood_exposure += f64::from(record.flood_peak);
        self.ph += f64::from(record.ph_delta);
        self.fertility += f64::from(record.fertility_delta);
        self.salinity += f64::from(record.salinity_delta);
        self.water += i64::from(record.net_water_delta_per_col);
    }

    fn from_archive(record: &ArchivedFluxRecord, start: u64, end: u64) -> Self {
        let start = start.max(record.start_step);
        let end = end.min(record.end_step);
        let len = record.end_step - record.start_step + 1;
        let fraction = (end - start + 1) as f64 / len as f64;
        // Normal archival seals runs at extant chunk cursors. A replay clipped by
        // incomplete publication prorates the totals and partitions integer water.
        let water_prefix =
            |offset| i128::from(record.water_per_col) * i128::from(offset) / i128::from(len);
        Self {
            start,
            end,
            era: record.era,
            archived: true,
            acid: record.acid * fraction,
            salt: record.salt * fraction,
            flood_exposure: record.flood_exposure * fraction,
            ph: record.ph * fraction,
            fertility: record.fertility * fraction,
            salinity: record.salinity * fraction,
            water: (water_prefix(end - record.start_step + 1)
                - water_prefix(start - record.start_step)) as i64,
        }
    }
}

struct FieldRelaxation {
    alpha: f64,
    ph: f64,
    fertility: f64,
    salinity: f64,
}

impl FieldRelaxation {
    fn new(role: BasinRole, bucket: &FluxBucket, days: f64, attenuation: f64) -> Self {
        let (ph, fertility, salt_factor) = match bucket.era {
            era if era == EraPhase::SaltCrustDesert as u8 => (7_550.0, 22_000.0, 1.18),
            era if era == EraPhase::SilentReverseRain as u8 => (7_300.0, 23_000.0, 1.10),
            era if era == EraPhase::RustRedRainFlood as u8 => (6_350.0, 29_000.0, 0.82),
            _ => (6_850.0, 27_000.0, 1.0),
        };
        let salinity = match role {
            BasinRole::ElectrolyteSaltBasin => 17_000.0,
            BasinRole::CanyonTravertineTerrace => 9_000.0,
            BasinRole::FaultBasin => 7_000.0,
            BasinRole::RidgePlateau => 5_000.0,
        };
        // Analytic relaxation under the interval's mean forcing. Both signs
        // use the same rounding in milli-pH.
        let elapsed = days * attenuation / 120.0;
        let alpha = -libm::expm1(-elapsed);
        let forcing = attenuation * (alpha / elapsed);
        Self {
            alpha,
            ph: ph * alpha + bucket.ph * 100.0 * forcing,
            fertility: fertility * alpha + bucket.fertility * forcing,
            salinity: salinity * salt_factor * alpha + bucket.salinity * forcing,
        }
    }

    fn apply(&self, l1: &mut FieldChunkSoA, i: usize) {
        let retain = 1.0 - self.alpha;
        l1.ph_milli[i] = (f64::from(l1.ph_milli[i]) * retain + self.ph)
            .clamp(2500.0, 12_500.0)
            .round() as u16;
        l1.fertility[i] = (f64::from(l1.fertility[i]) * retain + self.fertility)
            .clamp(0.0, 65_535.0)
            .round() as u16;
        l1.salinity[i] = (f64::from(l1.salinity[i]) * retain + self.salinity)
            .clamp(0.0, 65_535.0)
            .round() as u16;
    }
}

fn realize_surface_geology(
    world: &mut WorldState,
    region: usize,
    x: usize,
    role: BasinRole,
    bucket: &FluxBucket,
    steps_per_day: u64,
) {
    let reference = reference_height(world, x);
    let y = usize::from(world.topology.surface_height[x]);
    if y.abs_diff(reference) > 8
        || !world
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
            if y.abs_diff(reference) > 8 {
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

fn crosses_day(bucket: &FluxBucket, steps_per_day: u64, period: u64, phase: u64) -> bool {
    let offset = phase * steps_per_day;
    let span = period * steps_per_day;
    (bucket.end + offset) / span > (bucket.start - 1 + offset) / span
}

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

fn precipitate_column(world: &mut WorldState, x: usize, sy: usize, mut remaining: i64) -> i64 {
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

fn evaporate_column(world: &mut WorldState, x: usize, sy: usize, mut remaining: i64) -> i64 {
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

fn transpire_aquifer(
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
    chunk.dirty_rect.mark(x % CHUNK_SIZE, y % CHUNK_SIZE);
    chunk.sleep.fluid_sleeping = false;
    chunk.sleep.dirty_for_save = true;
}

/// Catch up the requested chunks within the current frame budget.
pub fn catch_up_active_chunks(
    world: &mut WorldState,
    active_mask: &[bool],
    max_chunks_budget: usize,
) -> usize {
    let mut caught_up = 0;
    let target_step = world.clock.macro_step.0;
    for (ci, &active) in active_mask.iter().enumerate() {
        if caught_up >= max_chunks_budget {
            break;
        }
        if active
            && world.l0_chunks[ci].last_realized_macro_step < target_step
            && catch_up_chunk(world, ci)
        {
            caught_up += 1;
        }
    }
    caught_up
}
