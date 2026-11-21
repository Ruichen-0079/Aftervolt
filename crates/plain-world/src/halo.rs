use rayon::prelude::*;

use crate::spatial::{
    FieldChunkSoA, WorldState, FLUX_BLOCK_E, FLUX_BLOCK_N, FLUX_BLOCK_S, FLUX_BLOCK_W,
};

/// One read-only tile at each open and ready chunk seam. Slots are N, S, E, W.
pub(crate) fn snapshot<T: Copy + Send>(
    world: &WorldState,
    ready: &[bool],
    read: impl Fn(&FieldChunkSoA, usize) -> T + Sync,
) -> Vec<Vec<Option<T>>> {
    let width = world.config.dimensions.chunks_x as usize;
    let height = world.config.dimensions.chunks_y as usize;
    let axis = world.config.field_downsample.tiles_per_chunk_axis();
    let edge = axis - 1;
    world
        .l1_chunks
        .par_iter()
        .enumerate()
        .map(|(ci, local)| {
            if !ready[ci] {
                return Vec::new();
            }
            let x = ci % width;
            let y = ci / width;
            let mut halo = vec![None; 4 * axis];
            let neighbors = [
                (y + 1 < height, ci + width, FLUX_BLOCK_N, FLUX_BLOCK_S),
                (y > 0, ci.wrapping_sub(width), FLUX_BLOCK_S, FLUX_BLOCK_N),
                (x + 1 < width, ci + 1, FLUX_BLOCK_E, FLUX_BLOCK_W),
                (x > 0, ci.wrapping_sub(1), FLUX_BLOCK_W, FLUX_BLOCK_E),
            ];
            for (side, (exists, ni, outgoing, incoming)) in neighbors.into_iter().enumerate() {
                if !exists || !ready[ni] {
                    continue;
                }
                let remote = &world.l1_chunks[ni];
                for k in 0..axis {
                    let (i, j) = match side {
                        0 => (edge * axis + k, k),
                        1 => (k, edge * axis + k),
                        2 => (k * axis + edge, k * axis),
                        _ => (k * axis, k * axis + edge),
                    };
                    if local.flux_block_mask[i] & outgoing == 0
                        && remote.flux_block_mask[j] & incoming == 0
                    {
                        halo[side * axis + k] = Some(read(remote, j));
                    }
                }
            }
            halo
        })
        .collect()
}
