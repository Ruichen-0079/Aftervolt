use serde::{Deserialize, Serialize};

/// Side length of one L0 chunk in cells (F4: 64x64 cells/chunk).
pub const CHUNK_SIZE: usize = 64;
/// Total L0 cells per chunk (4096).
pub const CHUNK_CELLS: usize = CHUNK_SIZE * CHUNK_SIZE;

/// Downsampling factor from L0 fine cells to L1 environment field tiles (F4 & Q1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum FieldDownsample {
    /// 2x2 L0 cells per L1 field tile (32x32 field tiles per chunk).
    By2 = 2,
    /// 4x4 L0 cells per L1 field tile (16x16 field tiles per chunk).
    By4 = 4,
}

impl FieldDownsample {
    #[inline]
    pub const fn factor(self) -> usize {
        self as usize
    }

    #[inline]
    pub const fn tiles_per_chunk_axis(self) -> usize {
        CHUNK_SIZE / (self as usize)
    }

    #[inline]
    pub const fn tiles_per_chunk(self) -> usize {
        let axis = self.tiles_per_chunk_axis();
        axis * axis
    }
}

/// Configurable finite 2D world bounds expressed in chunks (F3: never hardcoded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorldDimensions {
    pub chunks_x: u16,
    pub chunks_y: u16,
}

impl WorldDimensions {
    pub const fn from_chunks(chunks_x: u16, chunks_y: u16) -> Self {
        Self { chunks_x, chunks_y }
    }

    /// Compact benchmark preset: 4096 x 1536 cells (64 x 24 chunks).
    pub const fn preset_4096x1536() -> Self {
        Self {
            chunks_x: 64,
            chunks_y: 24,
        }
    }

    /// Primary candidate benchmark preset: 6144 x 2048 cells (96 x 32 chunks).
    pub const fn candidate_6144x2048() -> Self {
        Self {
            chunks_x: 96,
            chunks_y: 32,
        }
    }

    /// Large benchmark preset: 8192 x 2048 cells (128 x 32 chunks).
    pub const fn preset_8192x2048() -> Self {
        Self {
            chunks_x: 128,
            chunks_y: 32,
        }
    }

    /// Small fast-CI test preset: 1024 x 512 cells (16 x 8 chunks).
    pub const fn preset_ci_1024x512() -> Self {
        Self {
            chunks_x: 16,
            chunks_y: 8,
        }
    }

    #[inline]
    pub const fn width_cells(self) -> usize {
        (self.chunks_x as usize) * CHUNK_SIZE
    }

    #[inline]
    pub const fn height_cells(self) -> usize {
        (self.chunks_y as usize) * CHUNK_SIZE
    }

    #[inline]
    pub const fn total_chunks(self) -> usize {
        (self.chunks_x as usize) * (self.chunks_y as usize)
    }

    #[inline]
    pub const fn total_cells(self) -> usize {
        self.width_cells() * self.height_cells()
    }

    #[inline]
    pub const fn width_fields(self, ds: FieldDownsample) -> usize {
        (self.chunks_x as usize) * ds.tiles_per_chunk_axis()
    }

    #[inline]
    pub const fn height_fields(self, ds: FieldDownsample) -> usize {
        (self.chunks_y as usize) * ds.tiles_per_chunk_axis()
    }

    #[inline]
    pub fn contains_cell(self, c: CellCoord) -> bool {
        c.x >= 0
            && c.y >= 0
            && (c.x as usize) < self.width_cells()
            && (c.y as usize) < self.height_cells()
    }

    #[inline]
    pub fn contains_chunk(self, c: ChunkCoord) -> bool {
        c.cx >= 0
            && c.cy >= 0
            && (c.cx as usize) < (self.chunks_x as usize)
            && (c.cy as usize) < (self.chunks_y as usize)
    }

    #[inline]
    pub fn chunk_index(self, c: ChunkCoord) -> Option<usize> {
        if self.contains_chunk(c) {
            Some((c.cy as usize) * (self.chunks_x as usize) + (c.cx as usize))
        } else {
            None
        }
    }
}

/// Canonical physical world position in meters, decoupled from cell resolution (F3).
/// Coordinate convention: +X points right, +Y points upward (y = 0 at bedrock floor).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct WorldPosMeters {
    pub x: f64,
    pub y: f64,
}

impl WorldPosMeters {
    #[inline]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    #[inline]
    pub fn to_cell(self, meters_per_cell: f32) -> CellCoord {
        let inv = 1.0 / (meters_per_cell as f64);
        CellCoord {
            x: crate::rng::det_floor_f64(self.x * inv) as i32,
            y: crate::rng::det_floor_f64(self.y * inv) as i32,
        }
    }
}

/// Discrete L0 cell coordinate (+X right, +Y up from bedrock).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(C)]
pub struct CellCoord {
    pub x: i32,
    pub y: i32,
}

impl CellCoord {
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    #[inline]
    pub const fn chunk_coord(self) -> ChunkCoord {
        ChunkCoord {
            cx: self.x.div_euclid(CHUNK_SIZE as i32),
            cy: self.y.div_euclid(CHUNK_SIZE as i32),
        }
    }

    #[inline]
    pub const fn local_index(self) -> usize {
        let lx = self.x.rem_euclid(CHUNK_SIZE as i32) as usize;
        let ly = self.y.rem_euclid(CHUNK_SIZE as i32) as usize;
        ly * CHUNK_SIZE + lx
    }

    #[inline]
    pub fn to_world_meters_center(self, meters_per_cell: f32) -> WorldPosMeters {
        let s = meters_per_cell as f64;
        WorldPosMeters {
            x: (self.x as f64 + 0.5) * s,
            y: (self.y as f64 + 0.5) * s,
        }
    }
}

/// Discrete L2 chunk coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(C)]
pub struct ChunkCoord {
    pub cx: i32,
    pub cy: i32,
}

impl ChunkCoord {
    #[inline]
    pub const fn new(cx: i32, cy: i32) -> Self {
        Self { cx, cy }
    }
}

/// Discrete L1 field tile coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(C)]
pub struct FieldCoord {
    pub fx: i32,
    pub fy: i32,
}

impl FieldCoord {
    #[inline]
    pub const fn from_cell(cell: CellCoord, ds: FieldDownsample) -> Self {
        let f = ds.factor() as i32;
        Self {
            fx: cell.x.div_euclid(f),
            fy: cell.y.div_euclid(f),
        }
    }
}

/// L3 MacroRegion / Basin identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct RegionId(pub u16);

/// Globally unique persistent entity ID across saves, LODs, and playthroughs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PersistentUid(pub u64);

/// Static species prototype identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct SpeciesId(pub u16);

/// Social group / colony identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct GroupId(pub u32);
