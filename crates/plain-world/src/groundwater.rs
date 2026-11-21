use std::collections::VecDeque;

use plain_contracts::materials::{MaterialId, MATERIALS};
use plain_contracts::{CHUNK_CELLS, CHUNK_SIZE};

use crate::spatial::TerrainChunkSoA;

type Address = (usize, usize);
type Point = (usize, usize);

const PERMEABLE: [bool; 256] = {
    let mut table = [false; 256];
    let mut id = 0;
    while id < table.len() {
        table[id] = MATERIALS.get(MaterialId(id as u8)).permeability >= 100;
        id += 1;
    }
    table
};

/// Derived reachability from the atmospheric boundary through face-connected
/// air and permeable material. Water movements alone never invalidate it.
#[derive(Debug, Clone, Default)]
pub struct GroundwaterConnectivity {
    connected: Vec<[u64; CHUNK_SIZE]>,
    openings: Vec<Point>,
}

struct Terrain<'a> {
    chunks: &'a [TerrainChunkSoA],
    columns: usize,
}

impl Terrain<'_> {
    fn width(&self) -> usize {
        self.columns * CHUNK_SIZE
    }

    fn height(&self) -> usize {
        self.chunks.len() / self.columns * CHUNK_SIZE
    }

    fn address(&self, (x, y): Point) -> Address {
        (
            y / CHUNK_SIZE * self.columns + x / CHUNK_SIZE,
            y % CHUNK_SIZE * CHUNK_SIZE + x % CHUNK_SIZE,
        )
    }

    fn porous(&self, (ci, li): Address) -> bool {
        PERMEABLE[usize::from(self.chunks[ci].solid_material[li])]
    }

    fn neighbors(&self, (x, y): Point) -> impl Iterator<Item = Point> {
        [
            x.checked_sub(1).map(|nx| (nx, y)),
            (x + 1 < self.width()).then_some((x + 1, y)),
            y.checked_sub(1).map(|ny| (x, ny)),
            (y + 1 < self.height()).then_some((x, y + 1)),
        ]
        .into_iter()
        .flatten()
    }

    fn local_bypass(&self, (x, y): Point) -> bool {
        // Group the surviving cardinal neighbors by walking the ring around
        // the changed cell. If all are in one group, the closure cannot
        // disconnect any path. Treat uncertain closures as a lazy rebuild.
        let ring = [
            (-1, -1),
            (0, -1),
            (1, -1),
            (1, 0),
            (1, 1),
            (0, 1),
            (-1, 1),
            (-1, 0),
        ];
        let open = ring.map(|(dx, dy)| {
            x.checked_add_signed(dx)
                .zip(y.checked_add_signed(dy))
                .is_some_and(|p @ (x, y)| {
                    x < self.width() && y < self.height() && self.porous(self.address(p))
                })
        });
        let Some(gap) = open.iter().position(|&open| !open) else {
            return true;
        };
        let mut groups = 0;
        let mut connected_face = false;
        for offset in 1..=open.len() {
            let i = (gap + offset) % open.len();
            if !open[i] {
                connected_face = false;
            } else if i % 2 == 1 && !connected_face {
                connected_face = true;
                groups += 1;
            }
        }
        groups <= 1
    }
}

impl GroundwaterConnectivity {
    pub(crate) fn contains(&self, ci: usize, li: usize) -> bool {
        self.connected[ci][li / CHUNK_SIZE] & (1 << (li % CHUNK_SIZE)) != 0
    }

    pub(crate) fn terrain_changed(
        &mut self,
        chunks: &[TerrainChunkSoA],
        columns: usize,
        point: Point,
        before: MaterialId,
        after: MaterialId,
    ) {
        if self.connected.is_empty()
            || PERMEABLE[usize::from(before.0)] == PERMEABLE[usize::from(after.0)]
        {
            return;
        }
        if PERMEABLE[usize::from(after.0)] {
            self.openings.push(point);
            return;
        }

        let terrain = Terrain { chunks, columns };
        let (ci, li) = terrain.address(point);
        if !self.contains(ci, li) {
            return;
        }
        if point.1 + 1 == terrain.height() || !terrain.local_bypass(point) {
            self.connected.clear();
            self.openings.clear();
        } else {
            self.connected[ci][li / CHUNK_SIZE] &= !(1 << (li % CHUNK_SIZE));
        }
    }

    pub(crate) fn refresh(&mut self, chunks: &[TerrainChunkSoA], columns: usize) {
        let terrain = Terrain { chunks, columns };
        if self.connected.is_empty() {
            self.rebuild(&terrain);
            return;
        }
        if self.openings.is_empty() {
            return;
        }
        let mut queue = VecDeque::new();
        let top = terrain.height() - 1;
        let mut openings = std::mem::take(&mut self.openings);
        for point in openings.drain(..) {
            if point.1 == top
                || terrain.neighbors(point).any(|neighbor| {
                    let (ci, li) = terrain.address(neighbor);
                    self.contains(ci, li)
                })
            {
                self.visit(&terrain, point, &mut queue);
            }
        }
        self.openings = openings;
        while let Some(point) = queue.pop_front() {
            for neighbor in terrain.neighbors(point) {
                self.visit(&terrain, neighbor, &mut queue);
            }
        }
    }

    fn rebuild(&mut self, terrain: &Terrain<'_>) {
        // Flood whole porous runs using one word per L0 row. The large
        // aboveground air volume won't require a queue entry per cell.
        let total = terrain.chunks.len();
        let porous: Vec<[u64; CHUNK_SIZE]> = terrain
            .chunks
            .iter()
            .map(|chunk| {
                std::array::from_fn(|y| {
                    chunk.solid_material[y * CHUNK_SIZE..(y + 1) * CHUNK_SIZE]
                        .iter()
                        .enumerate()
                        .fold(0, |mask, (x, &id)| {
                            mask | (u64::from(PERMEABLE[usize::from(id)]) << x)
                        })
                })
            })
            .collect();
        self.connected.resize(total, [0; CHUNK_SIZE]);
        let mut queue = VecDeque::new();
        let mut enqueue = |ci: usize, y: usize, seeds: u64, queue: &mut VecDeque<_>| {
            let seen = &mut self.connected[ci][y];
            let open = porous[ci][y];
            let mut todo = seeds & open & !*seen;
            let mut reached = 0;
            while todo != 0 {
                let i = todo.trailing_zeros();
                let left = i + 1 - (open << (63 - i)).leading_ones();
                let right = i + (open >> i).trailing_ones();
                let run = (u64::MAX << left) & (u64::MAX >> (64 - right));
                reached |= run;
                todo &= !run;
            }
            if reached != 0 {
                *seen |= reached;
                queue.push_back((ci, y, reached));
            }
        };

        for ci in total - terrain.columns..total {
            enqueue(ci, CHUNK_SIZE - 1, u64::MAX, &mut queue);
        }
        while let Some((ci, y, mask)) = queue.pop_front() {
            if y > 0 {
                enqueue(ci, y - 1, mask, &mut queue);
            } else if ci >= terrain.columns {
                enqueue(ci - terrain.columns, CHUNK_SIZE - 1, mask, &mut queue);
            }
            if y + 1 < CHUNK_SIZE {
                enqueue(ci, y + 1, mask, &mut queue);
            } else if ci + terrain.columns < total {
                enqueue(ci + terrain.columns, 0, mask, &mut queue);
            }
            if mask & 1 != 0 && ci % terrain.columns > 0 {
                enqueue(ci - 1, y, 1 << 63, &mut queue);
            }
            if mask >> 63 != 0 && ci % terrain.columns + 1 < terrain.columns {
                enqueue(ci + 1, y, 1, &mut queue);
            }
        }
    }

    fn visit(&mut self, terrain: &Terrain<'_>, point: Point, queue: &mut VecDeque<Point>) {
        let (ci, li) = terrain.address(point);
        if terrain.porous((ci, li)) && !self.contains(ci, li) {
            self.connected[ci][li / CHUNK_SIZE] |= 1 << (li % CHUNK_SIZE);
            queue.push_back(point);
        }
    }

    pub(crate) fn estimated_bytes(&self) -> usize {
        self.connected.capacity() * (CHUNK_CELLS / 8)
            + self.openings.capacity() * std::mem::size_of::<Point>()
    }
}
