use std::collections::BTreeMap;
use std::ops::Range;

use plain_contracts::telemetry::EraPhase;
use plain_contracts::CHUNK_SIZE;
use rayon::prelude::*;

use crate::fields::MicrobialAdvance;
use crate::spatial::{BasinRole, FieldChunkSoA, MacroRegion, TerrainChunkSoA, WorldState};

mod buckets;
mod geology;
mod hydrology;

use buckets::{collect_buckets, published_through, FluxBucket};
use geology::{realize_surface_geology, reference_height};
use hydrology::{evaporate_column, precipitate_column, transpire_aquifer};

pub(crate) use hydrology::settle_basin_water;

/// Replay contiguous archived and detailed regional flux for one chunk.
pub fn catch_up_chunk(world: &mut WorldState, chunk_idx: usize) -> bool {
    let context = ReplayContext::new(world, u64::MAX);
    let realized = realize_chunk(world, chunk_idx, &context, &mut BTreeMap::new());
    if realized {
        refresh_realized_conductivity(world, &context, None);
    }
    realized
}

struct ReplayContext {
    target: u64,
    chunks_x: usize,
    steps_per_day: u64,
    ds: usize,
    ticks_per_macro: u64,
    ticks_per_meso: u64,
    ecology_tick: u64,
}

impl ReplayContext {
    fn new(world: &WorldState, ecology_tick: u64) -> Self {
        Self {
            target: world.clock.macro_step.0,
            chunks_x: world.config.dimensions.chunks_x as usize,
            steps_per_day: u64::from(world.config.clock.macro_steps_per_game_day.max(1)),
            ds: world.config.field_downsample.factor(),
            ticks_per_macro: world.clock.config.ticks_per_macro_step(),
            ticks_per_meso: u64::from(world.clock.config.meso_period_ticks.max(1)),
            ecology_tick,
        }
    }

    fn origin(&self, ci: usize) -> (usize, usize) {
        (
            ci % self.chunks_x * CHUNK_SIZE,
            ci / self.chunks_x * CHUNK_SIZE,
        )
    }

    fn meso_at(&self, macro_step: u64) -> u64 {
        macro_step
            .saturating_mul(self.ticks_per_macro)
            .min(self.ecology_tick)
            / self.ticks_per_meso
    }

    fn spans(
        &self,
        regions: &[MacroRegion],
        last: u64,
        x: usize,
    ) -> (u64, Vec<(usize, Range<usize>)>) {
        let mut realized = self.target;
        let mut spans = Vec::new();
        for (id, region) in regions.iter().enumerate() {
            let start = region.x_start.max(x);
            let end = region.x_end.min(x + CHUNK_SIZE);
            if start < end {
                realized = realized.min(published_through(&region.flux_history, last, self.target));
                spans.push((id, start..end));
            }
        }
        (realized, spans)
    }

    fn finish(&self, l0: &mut TerrainChunkSoA, l1: &mut FieldChunkSoA, realized: u64) {
        l1.microbial_meso_step = l1.microbial_meso_step.max(self.meso_at(realized));
        if l0.sleep.flux_mask_dirty {
            l0.recompute_flux_mask(l1, self.ds);
        }
        l0.last_realized_macro_step = realized;
        l0.sleep.dirty_for_save = true;
    }
}

type ReplayBuckets = BTreeMap<(usize, u64, u64), Vec<FluxBucket>>;

fn realize_chunk(
    world: &mut WorldState,
    ci: usize,
    context: &ReplayContext,
    memo: &mut ReplayBuckets,
) -> bool {
    let last = world.l0_chunks[ci].last_realized_macro_step;
    if last >= context.target {
        return false;
    }
    let (x_origin, y_origin) = context.origin(ci);
    let (realized, spans) = context.spans(&world.regions, last, x_origin);
    if realized == last || spans.is_empty() {
        return false;
    }
    if world.surface_reference_height.is_empty() {
        world
            .surface_reference_height
            .clone_from(&world.topology.surface_height);
    }

    for (id, columns) in spans {
        let region = &world.regions[id];
        let role = region.role;
        let moisture = region.moisture_index;
        let buckets = memo
            .entry((id, last, realized))
            .or_insert_with(|| collect_buckets(&region.flux_history, last, realized));
        for bucket in buckets {
            for x in columns.clone() {
                if reference_height(world, x) / CHUNK_SIZE != y_origin / CHUNK_SIZE {
                    continue;
                }
                realize_surface_geology(world, id, x, role, bucket, context.steps_per_day);
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
            apply_bucket_fields(
                context,
                &mut world.l0_chunks[ci],
                &mut world.l1_chunks[ci],
                &world.surface_reference_height,
                (x_origin, y_origin),
                &columns,
                bucket,
                role,
                moisture,
            );
        }
    }
    context.finish(&mut world.l0_chunks[ci], &mut world.l1_chunks[ci], realized);
    true
}

/// Realize surface owners in fixed order, then replay independent chunks in parallel.
/// Surface owners may cut across a vertical seam or exchange water with other chunks.
pub(crate) fn catch_up_world(world: &mut WorldState) {
    let context = ReplayContext::new(world, u64::MAX);
    if world.regions.is_empty()
        || world
            .l0_chunks
            .iter()
            .all(|chunk| chunk.last_realized_macro_step >= context.target)
    {
        return;
    }
    if world.surface_reference_height.is_empty() {
        world
            .surface_reference_height
            .clone_from(&world.topology.surface_height);
    }
    let mut owners = vec![false; world.l0_chunks.len()];
    for (x, &y) in world.surface_reference_height.iter().enumerate() {
        let ci = usize::from(y) / CHUNK_SIZE * context.chunks_x + x / CHUNK_SIZE;
        if let Some(owner) = owners.get_mut(ci) {
            *owner = true;
        }
    }

    let mut memo = BTreeMap::new();
    for (ci, &owner) in owners.iter().enumerate() {
        if owner {
            realize_chunk(world, ci, &context, &mut memo);
        }
    }

    let regions = &world.regions;
    let surface = &world.surface_reference_height;
    world
        .l0_chunks
        .par_iter_mut()
        .zip(&mut world.l1_chunks)
        .enumerate()
        .for_each_init(BTreeMap::new, |local_memo, (ci, (l0, l1))| {
            let last = l0.last_realized_macro_step;
            if last >= context.target {
                if last == context.target {
                    l0.refresh_replay_conductivity(l1, context.ds);
                }
                return;
            }
            if owners[ci] {
                return;
            }
            let origin = context.origin(ci);
            let (realized, spans) = context.spans(regions, last, origin.0);
            if realized == last || spans.is_empty() {
                return;
            }
            for (id, columns) in spans {
                let region = &regions[id];
                let key = (id, last, realized);
                let buckets = memo.get(&key).unwrap_or_else(|| {
                    local_memo
                        .entry(key)
                        .or_insert_with(|| collect_buckets(&region.flux_history, last, realized))
                });
                for bucket in buckets {
                    apply_bucket_fields(
                        &context,
                        l0,
                        l1,
                        surface,
                        origin,
                        &columns,
                        bucket,
                        region.role,
                        region.moisture_index,
                    );
                }
            }
            context.finish(l0, l1, realized);
        });
}

#[allow(clippy::too_many_arguments)]
fn apply_bucket_fields(
    context: &ReplayContext,
    l0: &mut TerrainChunkSoA,
    l1: &mut FieldChunkSoA,
    surface: &[u16],
    (x_origin, y_origin): (usize, usize),
    columns: &Range<usize>,
    bucket: &FluxBucket,
    role: BasinRole,
    latest_moisture: f32,
) {
    let elapsed = context.meso_at(bucket.end).saturating_sub(
        l1.microbial_meso_step
            .max(context.meso_at(bucket.start - 1)),
    );
    let advance = MicrobialAdvance::new(elapsed);
    let moisture = bucket.moisture.mean().unwrap_or(latest_moisture);
    let ds = context.ds;
    let axis = CHUNK_SIZE / ds;
    let days = (bucket.end - bucket.start + 1) as f64 / context.steps_per_day as f64;
    let shallow = FieldRelaxation::new(role, bucket, days, 1.0);
    let deep = FieldRelaxation::new(role, bucket, days, 0.18);
    // Assign L1 tiles by center even when a basin boundary cuts a chunk.
    let center = x_origin + ds / 2;
    let first = columns.start.saturating_sub(center).div_ceil(ds);
    let end = columns.end.saturating_sub(center).div_ceil(ds).min(axis);
    if first >= end {
        return;
    }
    let conditions = l0.tile_conditions(axis);
    for fy in 0..axis {
        let y = y_origin + fy * ds + ds / 2;
        for fx in first..end {
            let height = usize::from(surface.get(center + fx * ds).copied().unwrap_or(0));
            let relaxation = if y.abs_diff(height) <= 64 {
                &shallow
            } else {
                &deep
            };
            let fi = fy * axis + fx;
            relaxation.apply(l1, fi);
            advance.replay_tile(&conditions[fi], l1, fi, moisture);
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

/// Catch up the requested chunks within the current frame budget.
pub fn catch_up_active_chunks(
    world: &mut WorldState,
    active_mask: &[bool],
    max_chunks_budget: usize,
) -> usize {
    replay_active_chunks(world, active_mask, max_chunks_budget, u64::MAX)
}

pub(crate) fn catch_up_fine_tick(
    world: &mut WorldState,
    active_mask: &[bool],
    max_chunks_budget: usize,
) {
    replay_active_chunks(
        world,
        active_mask,
        max_chunks_budget,
        world.clock.tick.0 - 1,
    );
}

fn replay_active_chunks(
    world: &mut WorldState,
    active_mask: &[bool],
    max_chunks_budget: usize,
    ecology_tick: u64,
) -> usize {
    let mut caught_up = 0;
    let context = ReplayContext::new(world, ecology_tick);
    let mut memo = BTreeMap::new();
    for (ci, &active) in active_mask.iter().enumerate() {
        if caught_up >= max_chunks_budget {
            break;
        }
        if active
            && world.l0_chunks[ci].last_realized_macro_step < context.target
            && realize_chunk(world, ci, &context, &mut memo)
        {
            caught_up += 1;
        }
    }
    if caught_up != 0 {
        refresh_realized_conductivity(world, &context, Some(active_mask));
    }
    caught_up
}

fn refresh_realized_conductivity(
    world: &mut WorldState,
    context: &ReplayContext,
    active: Option<&[bool]>,
) {
    // A surface owner may change a neighboring chunk whose replay already
    // finished, e.g. by eroding down through a seam or tapping a connected aquifer.
    for (ci, (l0, l1)) in world
        .l0_chunks
        .iter_mut()
        .zip(&mut world.l1_chunks)
        .enumerate()
    {
        if active.is_none_or(|mask| mask[ci]) && l0.last_realized_macro_step == context.target {
            l0.refresh_replay_conductivity(l1, context.ds);
        }
    }
}
