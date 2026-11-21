use plain_contracts::materials::{FluidKind, MaterialId};
use plain_contracts::{ChunkCoord, CELL_MAX_FLUID_UNITS, CHUNK_SIZE};
use rayon::prelude::*;

use crate::spatial::{DirtyRect, TerrainChunkSoA, WorldState};

/// Step cellular fluid flow across a set of active chunk indices using:
/// 1) Parallel intra-chunk dirty-rect cellular updates (`rayon::par_iter_mut`, 100% race-free).
/// 2) Deterministic 4-phase seam transfer across horizontal and vertical chunk borders.
///
/// Conservation guarantee: Every transfer subtracts exact `delta: u16` from source and adds exact
/// `delta: u16` to destination. Domain boundaries (outside finite world) are strict Neumann zero-flux.
pub fn step_fluid_active_chunks(
    world: &mut WorldState,
    active_mask: &[bool],
    tick_parity: u64,
) -> u32 {
    let chunks_x = world.config.dimensions.chunks_x as usize;
    let chunks_y = world.config.dimensions.chunks_y as usize;
    let flip_lr = (tick_parity & 1) == 1;

    // Phase A: Parallel interior fluid cellular step on all active non-sleeping chunks.
    let moved_interior: u32 = world
        .l0_chunks
        .par_iter_mut()
        .enumerate()
        .map(|(ci, chunk)| {
            if !active_mask[ci] || chunk.sleep.fluid_sleeping || !chunk.dirty_rect.active {
                return 0u32;
            }
            step_chunk_interior_fluid(chunk, flip_lr)
        })
        .sum();

    // Phase B: Deterministic cross-chunk border transfers (bottom->top vertical seams, then L<->R horizontal seams).
    // Only touches the 64-cell border strips between adjacent chunks where both chunks are active and at least one is awake.
    let mut border_moves = 0u32;

    // B.1 Vertical seams: fluid falling from cy into cy - 1 (y=0 of upper -> y=63 of lower).
    for cy in 1..chunks_y {
        for cx in 0..chunks_x {
            let upper_idx = cy * chunks_x + cx;
            let lower_idx = (cy - 1) * chunks_x + cx;
            if (!active_mask[upper_idx] || !active_mask[lower_idx])
                || (world.l0_chunks[upper_idx].sleep.fluid_sleeping
                    && world.l0_chunks[lower_idx].sleep.fluid_sleeping)
            {
                continue;
            }
            // Safe split_at_mut since lower_idx < upper_idx
            let (low_slice, high_slice) = world.l0_chunks.split_at_mut(upper_idx);
            let lower = &mut low_slice[lower_idx];
            let upper = &mut high_slice[0];

            let mut seam_moved = false;
            for lx in 0..CHUNK_SIZE {
                let u_i = lx; // ly = 0
                let l_i = (CHUNK_SIZE - 1) * CHUNK_SIZE + lx; // ly = 63
                if upper.solid_material[u_i] != MaterialId::AIR.0
                    || lower.solid_material[l_i] != MaterialId::AIR.0
                {
                    continue;
                }
                let u_vol = upper.fluid_volume[u_i];
                if u_vol == 0 {
                    continue;
                }
                let l_vol = lower.fluid_volume[l_i];
                let space = CELL_MAX_FLUID_UNITS.saturating_sub(l_vol);
                if space > 0 {
                    let delta = u_vol.min(space);
                    upper.fluid_volume[u_i] -= delta;
                    lower.fluid_volume[l_i] += delta;
                    if lower.fluid_kind[l_i] == FluidKind::None as u8 || delta > l_vol {
                        lower.fluid_kind[l_i] = upper.fluid_kind[u_i];
                    }
                    if upper.fluid_volume[u_i] == 0 {
                        upper.fluid_kind[u_i] = FluidKind::None as u8;
                    }
                    upper.dirty_rect.mark(lx, 0);
                    lower.dirty_rect.mark(lx, CHUNK_SIZE - 1);
                    seam_moved = true;
                    border_moves += 1;
                }
            }
            if seam_moved {
                upper.sleep.fluid_sleeping = false;
                lower.sleep.fluid_sleeping = false;
                upper.sleep.dirty_for_save = true;
                lower.sleep.dirty_for_save = true;
            }
        }
    }

    // B.2 Horizontal seams: lateral leveling between (cx, cy) x=63 and (cx+1, cy) x=0.
    // Alternating even/odd cx parity avoids directional bias.
    for phase in 0..2 {
        let start_cx = if flip_lr { 1 - phase } else { phase };
        for cy in 0..chunks_y {
            let mut cx = start_cx;
            while cx + 1 < chunks_x {
                let left_idx = cy * chunks_x + cx;
                let right_idx = left_idx + 1;
                if (active_mask[left_idx] && active_mask[right_idx])
                    && (!world.l0_chunks[left_idx].sleep.fluid_sleeping
                        || !world.l0_chunks[right_idx].sleep.fluid_sleeping)
                {
                    let (l_slice, r_slice) = world.l0_chunks.split_at_mut(right_idx);
                    let left = &mut l_slice[left_idx];
                    let right = &mut r_slice[0];
                    let mut seam_moved = false;

                    for ly in 0..CHUNK_SIZE {
                        let li = ly * CHUNK_SIZE + (CHUNK_SIZE - 1);
                        let ri = ly * CHUNK_SIZE;
                        if left.solid_material[li] != MaterialId::AIR.0
                            || right.solid_material[ri] != MaterialId::AIR.0
                        {
                            continue;
                        }
                        let lv = left.fluid_volume[li];
                        let rv = right.fluid_volume[ri];
                        if lv > rv + 8 {
                            let delta = ((lv - rv) / 3).max(1);
                            left.fluid_volume[li] -= delta;
                            right.fluid_volume[ri] += delta;
                            if right.fluid_kind[ri] == 0 || delta > rv {
                                right.fluid_kind[ri] = left.fluid_kind[li];
                            }
                            if left.fluid_volume[li] == 0 {
                                left.fluid_kind[li] = 0;
                            }
                            left.dirty_rect.mark(CHUNK_SIZE - 1, ly);
                            right.dirty_rect.mark(0, ly);
                            seam_moved = true;
                            border_moves += 1;
                        } else if rv > lv + 8 {
                            let delta = ((rv - lv) / 3).max(1);
                            right.fluid_volume[ri] -= delta;
                            left.fluid_volume[li] += delta;
                            if left.fluid_kind[li] == 0 || delta > lv {
                                left.fluid_kind[li] = right.fluid_kind[ri];
                            }
                            if right.fluid_volume[ri] == 0 {
                                right.fluid_kind[ri] = 0;
                            }
                            left.dirty_rect.mark(CHUNK_SIZE - 1, ly);
                            right.dirty_rect.mark(0, ly);
                            seam_moved = true;
                            border_moves += 1;
                        }
                    }

                    if seam_moved {
                        left.sleep.fluid_sleeping = false;
                        right.sleep.fluid_sleeping = false;
                        left.sleep.dirty_for_save = true;
                        right.sleep.dirty_for_save = true;
                    }
                }
                cx += 2;
            }
        }
    }

    moved_interior + border_moves
}

/// Interior bottom-up cellular fluid pass within one 64x64 chunk.
/// Evaluates only the expanded dirty rectangle; puts chunk back to sleep when static.
fn step_chunk_interior_fluid(chunk: &mut TerrainChunkSoA, flip_lr: bool) -> u32 {
    let mut rect = chunk.dirty_rect;
    rect.expand_by(1);
    let min_x = rect.min_x as usize;
    let max_x = rect.max_x as usize;
    let min_y = rect.min_y as usize;
    let max_y = rect.max_y as usize;

    let mut next_dirty = DirtyRect::empty();
    let mut moves = 0u32;

    // Bottom-up scan prevents water teleporting across a full column in 1 tick.
    for ly in min_y..=max_y {
        for step_x in 0..=(max_x - min_x) {
            let lx = if flip_lr {
                max_x - step_x
            } else {
                min_x + step_x
            };
            let idx = ly * CHUNK_SIZE + lx;
            if chunk.solid_material[idx] != MaterialId::AIR.0 {
                continue;
            }
            let mut vol = chunk.fluid_volume[idx];
            if vol == 0 {
                continue;
            }
            let fkind = chunk.fluid_kind[idx];

            // 1. Gravity drop straight down (ly - 1)
            if ly > 0 {
                let below = (ly - 1) * CHUNK_SIZE + lx;
                if chunk.solid_material[below] == MaterialId::AIR.0 {
                    let bvol = chunk.fluid_volume[below];
                    let space = CELL_MAX_FLUID_UNITS.saturating_sub(bvol);
                    if space > 0 {
                        let delta = vol.min(space);
                        vol -= delta;
                        chunk.fluid_volume[idx] = vol;
                        chunk.fluid_volume[below] = bvol + delta;
                        if chunk.fluid_kind[below] == 0 || delta > bvol {
                            chunk.fluid_kind[below] = fkind;
                        }
                        if vol == 0 {
                            chunk.fluid_kind[idx] = 0;
                        }
                        next_dirty.mark(lx, ly);
                        next_dirty.mark(lx, ly - 1);
                        moves += 1;
                        if vol == 0 {
                            continue;
                        }
                    }
                }
            }

            // 2. Diagonal downhill slide (ly - 1, lx +- 1)
            if ly > 0 && vol > 16 {
                let dirs: [isize; 2] = if flip_lr { [1, -1] } else { [-1, 1] };
                for dx in dirs {
                    let nx = lx as isize + dx;
                    if nx < 0 || nx >= CHUNK_SIZE as isize {
                        continue;
                    }
                    let nxu = nx as usize;
                    let diag = (ly - 1) * CHUNK_SIZE + nxu;
                    if chunk.solid_material[diag] == MaterialId::AIR.0 {
                        let dvol = chunk.fluid_volume[diag];
                        let space = CELL_MAX_FLUID_UNITS.saturating_sub(dvol);
                        if space > 0 {
                            let delta = (vol / 2).max(1).min(space);
                            vol -= delta;
                            chunk.fluid_volume[idx] = vol;
                            chunk.fluid_volume[diag] = dvol + delta;
                            if chunk.fluid_kind[diag] == 0 || delta > dvol {
                                chunk.fluid_kind[diag] = fkind;
                            }
                            if vol == 0 {
                                chunk.fluid_kind[idx] = 0;
                            }
                            next_dirty.mark(lx, ly);
                            next_dirty.mark(nxu, ly - 1);
                            moves += 1;
                            if vol <= 16 {
                                break;
                            }
                        }
                    }
                }
            }

            // 3. Lateral leveling (lx +- 1)
            if vol > 12 {
                let dirs: [isize; 2] = if flip_lr { [1, -1] } else { [-1, 1] };
                for dx in dirs {
                    let nx = lx as isize + dx;
                    if nx < 0 || nx >= CHUNK_SIZE as isize {
                        continue;
                    }
                    let nxu = nx as usize;
                    let side = ly * CHUNK_SIZE + nxu;
                    if chunk.solid_material[side] == MaterialId::AIR.0 {
                        let svol = chunk.fluid_volume[side];
                        if vol > svol + 10 {
                            let delta = ((vol - svol) / 3).max(1);
                            vol -= delta;
                            chunk.fluid_volume[idx] = vol;
                            chunk.fluid_volume[side] = svol + delta;
                            if chunk.fluid_kind[side] == 0 || delta > svol {
                                chunk.fluid_kind[side] = fkind;
                            }
                            if vol == 0 {
                                chunk.fluid_kind[idx] = 0;
                            }
                            next_dirty.mark(lx, ly);
                            next_dirty.mark(nxu, ly);
                            moves += 1;
                            if vol <= 12 {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    chunk.sleep.dirty_for_save = true;
    if moves == 0 {
        chunk.dirty_rect.clear();
        chunk.sleep.fluid_sleeping = true;
    } else {
        chunk.dirty_rect = next_dirty;
    }
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
