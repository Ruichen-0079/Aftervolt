use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::{ChunkCoord, CELL_MAX_FLUID_UNITS, CHUNK_SIZE};
use rayon::prelude::*;

use crate::spatial::{DirtyRect, TerrainChunkSoA, WorldState};

const MIN_DIAGONAL_VOLUME: u16 = 16;
const MIN_LEVELING_VOLUME: u16 = 12;
const MIN_LEVELING_HEAD: u16 = 10;
const PARALLEL_MIN_DIRTY_CELLS: usize = 32 * CHUNK_SIZE * CHUNK_SIZE;

/// Update interior sources in parallel, then visit seam sources in deterministic
/// bottom-up order. A seam source evaluates all its neighbors with the same
/// gravity, diagonal and leveling priorities as an interior source.
pub fn step_fluid_active_chunks(
    world: &mut WorldState,
    active_mask: &[bool],
    tick_parity: u64,
) -> u32 {
    if world.l0_chunks.is_empty() {
        return 0;
    }
    let chunks_x = world.config.dimensions.chunks_x as usize;
    let chunks_y = world.config.dimensions.chunks_y as usize;
    let flip_lr = (tick_parity & 1) == 1;

    let mut dirty_cells = 0;
    let awake: Vec<_> = world
        .l0_chunks
        .iter_mut()
        .zip(active_mask)
        .map(|(chunk, &active)| {
            let was_active = std::mem::replace(&mut chunk.fluid_active_last_tick, active);
            let awake = active && !chunk.sleep.fluid_sleeping;
            if awake && chunk.dirty_rect.active {
                let mut rect = chunk.dirty_rect;
                rect.expand_by(1);
                dirty_cells += usize::from(rect.max_x - rect.min_x + 1)
                    * usize::from(rect.max_y - rect.min_y + 1);
            }
            active && (awake || !was_active)
        })
        .collect();
    let directions = neighbors(flip_lr);
    let mut moved_interior = 0;
    let step = |(ci, chunk): (usize, &mut TerrainChunkSoA)| {
        if !active_mask[ci] {
            return (0, SeamSources::default());
        }
        let cx = ci % chunks_x;
        let cy = ci / chunks_x;
        let moves = if !chunk.sleep.fluid_sleeping && chunk.dirty_rect.active {
            step_chunk_interior_fluid(chunk, flip_lr)
        } else {
            0
        };
        let rows = cy.saturating_sub(1)..=(cy + 1).min(chunks_y - 1);
        let columns = cx.saturating_sub(1)..=(cx + 1).min(chunks_x - 1);
        let sources = if awake[ci]
            || rows
                .into_iter()
                .any(|y| columns.clone().any(|x| awake[y * chunks_x + x]))
        {
            SeamSources::scan(chunk)
        } else {
            SeamSources::default()
        };
        (moves, sources)
    };
    // Sparse settling often touches less than a few full chunks. Avoid a
    // Rayon barrier every fine tick when the remaining work is small.
    let sources: Vec<_> = if dirty_cells < PARALLEL_MIN_DIRTY_CELLS {
        world.l0_chunks.iter_mut().enumerate().map(step).collect()
    } else {
        world
            .l0_chunks
            .par_iter_mut()
            .with_min_len(8)
            .enumerate()
            .map(step)
            .collect()
    };

    let mut seam_moves = 0;
    let mut candidates = Vec::with_capacity(chunks_x);
    for (cy, row) in sources.chunks_exact(chunks_x).enumerate() {
        candidates.clear();
        let mut rows = 0;
        for ordinal in 0..chunks_x {
            let cx = if flip_lr {
                chunks_x - 1 - ordinal
            } else {
                ordinal
            };
            let (moves, source) = row[cx];
            moved_interior += moves;
            let wet_rows = source.left | source.right | u64::from(source.bottom != 0);
            if wet_rows != 0 {
                rows |= wet_rows;
                candidates.push((cx, source));
            }
        }

        // Use the Phase A wet strips as the source list. New arrivals to a
        // dry strip start flowing on the next tick.
        while rows != 0 {
            let ly = rows.trailing_zeros() as usize;
            rows &= rows - 1;
            for &(cx, source) in &candidates {
                let ci = cy * chunks_x + cx;
                let mut columns = source.columns(ly);
                while columns != 0 {
                    let lx = if flip_lr {
                        63 - columns.leading_zeros()
                    } else {
                        columns.trailing_zeros()
                    } as usize;
                    columns &= !(1 << lx);
                    let src = ly * CHUNK_SIZE + lx;
                    let chunk = &world.l0_chunks[ci];
                    if chunk.fluid_volume[src] == 0
                        || chunk.solid_material[src] != MaterialId::AIR.0
                    {
                        continue;
                    }
                    let mut volume = chunk.fluid_volume[src];
                    let kind = chunk.fluid_kind[src];
                    for (dx, dy, flow) in directions {
                        if volume <= flow.minimum() {
                            continue;
                        }
                        let Some(x) = (cx * CHUNK_SIZE + lx).checked_add_signed(dx) else {
                            continue;
                        };
                        let Some(y) = (cy * CHUNK_SIZE + ly).checked_add_signed(dy) else {
                            continue;
                        };
                        if x >= chunks_x * CHUNK_SIZE {
                            continue;
                        }
                        let dc = y / CHUNK_SIZE * chunks_x + x / CHUNK_SIZE;
                        if !active_mask[dc] {
                            continue;
                        }
                        let dest = y % CHUNK_SIZE * CHUNK_SIZE + x % CHUNK_SIZE;
                        let target = &world.l0_chunks[dc];
                        if target.solid_material[dest] != MaterialId::AIR.0 {
                            continue;
                        }
                        let delta = flow.delta(volume, target.fluid_volume[dest]);
                        if delta == 0 {
                            continue;
                        }
                        withdraw(&mut world.l0_chunks[ci], src, delta);
                        receive(&mut world.l0_chunks[dc], dest, delta, kind);
                        mark_seam_change(&mut world.l0_chunks[ci], lx, ly);
                        mark_seam_change(&mut world.l0_chunks[dc], x % CHUNK_SIZE, y % CHUNK_SIZE);
                        seam_moves += 1;
                        volume -= delta;
                        if volume == 0 {
                            break;
                        }
                    }
                }
            }
        }
    }
    moved_interior + seam_moves
}

#[derive(Clone, Copy, Default)]
struct SeamSources {
    bottom: u64,
    left: u64,
    right: u64,
}

impl SeamSources {
    fn scan(chunk: &TerrainChunkSoA) -> Self {
        let wet = |index: usize| u64::from(chunk.fluid_volume[index] > 0);
        let mut source = Self::default();
        for x in 0..CHUNK_SIZE {
            source.bottom |= wet(x) << x;
        }
        for y in 1..CHUNK_SIZE {
            source.left |= wet(y * CHUNK_SIZE) << y;
            source.right |= wet((y + 1) * CHUNK_SIZE - 1) << y;
        }
        source
    }

    fn columns(self, y: usize) -> u64 {
        if y == 0 {
            self.bottom
        } else {
            let left = (self.left >> y) & 1;
            let right = (self.right >> y) & 1;
            left | (right << 63)
        }
    }
}

#[derive(Clone, Copy)]
enum Flow {
    Gravity,
    Diagonal,
    Level,
}

impl Flow {
    fn minimum(self) -> u16 {
        match self {
            Self::Gravity => 0,
            Self::Diagonal => MIN_DIAGONAL_VOLUME,
            Self::Level => MIN_LEVELING_VOLUME,
        }
    }

    fn delta(self, source: u16, target: u16) -> u16 {
        let space = CELL_MAX_FLUID_UNITS.saturating_sub(target);
        match self {
            Self::Gravity => source.min(space),
            Self::Diagonal if source > self.minimum() => (source / 2).min(space),
            Self::Level
                if source > self.minimum() && source.saturating_sub(target) > MIN_LEVELING_HEAD =>
            {
                (source - target) / 3
            }
            _ => 0,
        }
    }
}

fn neighbors(flip_lr: bool) -> [(isize, isize, Flow); 5] {
    let first = if flip_lr { 1 } else { -1 };
    [
        (0, -1, Flow::Gravity),
        (first, -1, Flow::Diagonal),
        (-first, -1, Flow::Diagonal),
        (first, 0, Flow::Level),
        (-first, 0, Flow::Level),
    ]
}

fn withdraw(chunk: &mut TerrainChunkSoA, src: usize, delta: u16) {
    chunk.fluid_volume[src] -= delta;
    if chunk.fluid_volume[src] == 0 {
        chunk.fluid_kind[src] = FluidKind::None as u8;
    }
}

fn receive(chunk: &mut TerrainChunkSoA, dest: usize, delta: u16, kind: u8) {
    let previous = chunk.fluid_volume[dest];
    chunk.fluid_volume[dest] += delta;
    if chunk.fluid_kind[dest] == FluidKind::None as u8 || delta > previous {
        chunk.fluid_kind[dest] = kind;
    }
}

fn mark_seam_change(chunk: &mut TerrainChunkSoA, x: usize, y: usize) {
    chunk.tile_conditions.clear();
    chunk.dirty_rect.mark(x, y);
    chunk.sleep.fluid_sleeping = false;
    chunk.sleep.dirty_for_save = true;
}

fn step_chunk_interior_fluid(chunk: &mut TerrainChunkSoA, flip_lr: bool) -> u32 {
    let mut rect = chunk.dirty_rect;
    rect.expand_by(1);
    // Sources on the bottom row or side strips run in the seam pass, even
    // when one of their possible destinations stays inside the chunk.
    let min_x = usize::from(rect.min_x).max(1);
    let max_x = usize::from(rect.max_x).min(CHUNK_SIZE - 2);
    let min_y = usize::from(rect.min_y).max(1);
    let max_y = usize::from(rect.max_y);
    let side_dirs = if flip_lr { [1, -1] } else { [-1, 1] };
    let mut next_dirty = DirtyRect::empty();
    let mut moves = 0;

    for ly in min_y..=max_y {
        for ordinal in min_x..=max_x {
            let lx = if flip_lr {
                min_x + max_x - ordinal
            } else {
                ordinal
            };
            let src = ly * CHUNK_SIZE + lx;
            let mut vol = chunk.fluid_volume[src];
            if vol == 0 || chunk.solid_material[src] != MaterialId::AIR.0 {
                continue;
            }
            let kind = chunk.fluid_kind[src];
            let mut transfer = |dx, dy, flow: Flow, current| {
                let nx = lx.wrapping_add_signed(dx);
                let ny = ly.wrapping_add_signed(dy);
                let dest = ny * CHUNK_SIZE + nx;
                if chunk.solid_material[dest] != MaterialId::AIR.0 {
                    return 0;
                }
                let delta = flow.delta(current, chunk.fluid_volume[dest]);
                if delta > 0 {
                    withdraw(chunk, src, delta);
                    receive(chunk, dest, delta, kind);
                    next_dirty.mark(lx, ly);
                    next_dirty.mark(nx, ny);
                    moves += 1;
                }
                delta
            };

            vol -= transfer(0, -1, Flow::Gravity, vol);
            for (flow, dy) in [(Flow::Diagonal, -1), (Flow::Level, 0)] {
                for dx in side_dirs {
                    if vol <= flow.minimum() {
                        break;
                    }
                    vol -= transfer(dx, dy, flow, vol);
                }
            }
        }
    }

    chunk.sleep.dirty_for_save = true;
    if moves > 0 {
        chunk.tile_conditions.clear();
    }
    chunk.sleep.fluid_sleeping = moves == 0;
    chunk.dirty_rect = next_dirty;
    moves
}

/// Wake fluid simulation across a rectangular chunk range.
pub fn wake_fluid_chunks(world: &mut WorldState, min_c: ChunkCoord, max_c: ChunkCoord) {
    let cx0 = min_c.cx.max(0);
    let cy0 = min_c.cy.max(0);
    let cx1 = max_c.cx.min(world.config.dimensions.chunks_x as i32 - 1);
    let cy1 = max_c.cy.min(world.config.dimensions.chunks_y as i32 - 1);
    for cy in cy0..=cy1 {
        for cx in cx0..=cx1 {
            if let Some(ci) = world.chunk_index(ChunkCoord::new(cx, cy)) {
                let chunk = &mut world.l0_chunks[ci];
                if chunk.sleep.fluid_sleeping || chunk.dirty_rect != DirtyRect::full() {
                    chunk.sleep.fluid_sleeping = false;
                    chunk.dirty_rect = DirtyRect::full();
                    chunk.sleep.dirty_for_save = true;
                }
            }
        }
    }
}
