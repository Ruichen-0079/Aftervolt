use serde::{Deserialize, Serialize};

/// Compact L0 solid material identifier (1 byte/cell keeps L0 chunk compact at 8 bytes/cell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct MaterialId(pub u8);

impl MaterialId {
    pub const AIR: Self = Self(0);
    pub const BASALT_BEDROCK: Self = Self(1);
    pub const GRANITE: Self = Self(2);
    pub const SANDSTONE: Self = Self(3);
    pub const TRAVERTINE: Self = Self(4);
    pub const SALT_CRUST: Self = Self(5);
    pub const LOOSE_SOIL_DAM: Self = Self(6);
    pub const SILT_CLAY: Self = Self(7);
    pub const MINERALIZED_BONE: Self = Self(8);
    pub const CONDUCTIVE_ORE: Self = Self(9);
    pub const RUIN_ALLOY_WALL: Self = Self(10);
    pub const RUIN_CONDUIT_CASING: Self = Self(11);
    pub const RUIN_HEAT_CORE: Self = Self(12);

    pub const COUNT: usize = 13;

    #[inline]
    pub const fn is_air(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub const fn is_solid(self) -> bool {
        self.0 != 0
    }

    #[inline]
    pub const fn is_ruin_material(self) -> bool {
        matches!(self.0, 10..=12)
    }
}

/// Distinct natural and ruin-influenced fluid classifications in Electroruin Plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum FluidKind {
    #[default]
    None = 0,
    RainPool = 1,
    TravertinePool = 2,
    SaltMarsh = 3,
    ThunderRiver = 4,
    BlackWater = 5,
}

impl FluidKind {
    #[inline]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::RainPool,
            2 => Self::TravertinePool,
            3 => Self::SaltMarsh,
            4 => Self::ThunderRiver,
            5 => Self::BlackWater,
            _ => Self::None,
        }
    }

    /// Baseline fluid conductivity contribution (scaled 0..=1000).
    #[inline]
    pub const fn base_conductivity(self) -> u16 {
        match self {
            Self::None => 0,
            Self::RainPool => 180,
            Self::TravertinePool => 340,
            Self::SaltMarsh => 820,
            Self::ThunderRiver => 760,
            Self::BlackWater => 680,
        }
    }
}

/// Purely physical material definition (F5: NO `indestructible` flag anywhere!).
/// Terrain modification, acid dissolution, collapse, and future player/creature interaction
/// all evaluate strictly against these continuous physical properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialDef {
    pub id: MaterialId,
    pub name: &'static str,
    /// Bulk density in kg/m^3.
    pub density_kg_m3: u32,
    /// Mechanical yield strength in kPa. High values resist weak tools/floods naturally.
    pub yield_strength_kpa: u32,
    /// Acid susceptibility (0..=1000). Determines erosion rate under RustRedRain or organic acids.
    pub acid_susceptibility: u16,
    /// Electro-thermal shock resistance (0..=1000). Lower values spall under lightning/arcs.
    pub electro_thermal_res: u16,
    /// Hydraulic permeability (0..=1000). Controls seepage into aquifers.
    pub permeability: u16,
    /// Electrical conductivity (0..=1000). Feeds L1 electric field solver directly.
    pub conductivity: u16,
    /// Structural cohesion (0..=1000). Supports overhanging arches/pillars against collapse.
    pub cohesion: u16,
    /// Default initial cell integrity (max structural HP before mechanical/chemical breakdown).
    pub base_integrity: u16,
    /// Secondary material left behind after weathering/erosion (None = dissolves to air/solute).
    pub erosion_product: Option<MaterialId>,
    /// RGB preview color for developer observer diagnostics.
    pub debug_rgb: [u8; 3],
}

/// Static read-only registry of physical materials.
pub struct MaterialRegistry {
    defs: [MaterialDef; MaterialId::COUNT],
}

impl MaterialRegistry {
    pub const fn default_registry() -> Self {
        Self {
            defs: [
                MaterialDef {
                    id: MaterialId::AIR,
                    name: "Air",
                    density_kg_m3: 1,
                    yield_strength_kpa: 0,
                    acid_susceptibility: 0,
                    electro_thermal_res: 0,
                    permeability: 1000,
                    conductivity: 5,
                    cohesion: 0,
                    base_integrity: 0,
                    erosion_product: None,
                    debug_rgb: [16, 20, 28],
                },
                MaterialDef {
                    id: MaterialId::BASALT_BEDROCK,
                    name: "BasaltBedrock",
                    density_kg_m3: 2950,
                    yield_strength_kpa: 280_000,
                    acid_susceptibility: 2,
                    electro_thermal_res: 960,
                    permeability: 4,
                    conductivity: 35,
                    cohesion: 980,
                    base_integrity: 60_000,
                    erosion_product: Some(MaterialId::GRANITE),
                    debug_rgb: [42, 44, 52],
                },
                MaterialDef {
                    id: MaterialId::GRANITE,
                    name: "Granite",
                    density_kg_m3: 2700,
                    yield_strength_kpa: 180_000,
                    acid_susceptibility: 12,
                    electro_thermal_res: 880,
                    permeability: 15,
                    conductivity: 45,
                    cohesion: 900,
                    base_integrity: 42_000,
                    erosion_product: Some(MaterialId::SANDSTONE),
                    debug_rgb: [76, 78, 86],
                },
                MaterialDef {
                    id: MaterialId::SANDSTONE,
                    name: "Sandstone",
                    density_kg_m3: 2300,
                    yield_strength_kpa: 48_000,
                    acid_susceptibility: 95,
                    electro_thermal_res: 620,
                    permeability: 140,
                    conductivity: 60,
                    cohesion: 650,
                    base_integrity: 18_000,
                    erosion_product: Some(MaterialId::LOOSE_SOIL_DAM),
                    debug_rgb: [152, 122, 86],
                },
                MaterialDef {
                    id: MaterialId::TRAVERTINE,
                    name: "Travertine",
                    density_kg_m3: 2450,
                    yield_strength_kpa: 56_000,
                    acid_susceptibility: 160,
                    electro_thermal_res: 700,
                    permeability: 35,
                    conductivity: 120,
                    cohesion: 740,
                    base_integrity: 22_000,
                    erosion_product: Some(MaterialId::SILT_CLAY),
                    debug_rgb: [198, 186, 152],
                },
                MaterialDef {
                    id: MaterialId::SALT_CRUST,
                    name: "SaltCrust",
                    density_kg_m3: 2160,
                    yield_strength_kpa: 14_000,
                    acid_susceptibility: 860,
                    electro_thermal_res: 400,
                    permeability: 20,
                    conductivity: 260,
                    cohesion: 420,
                    base_integrity: 4_500,
                    erosion_product: None, // Dissolves directly into aqueous salinity
                    debug_rgb: [228, 232, 236],
                },
                MaterialDef {
                    id: MaterialId::LOOSE_SOIL_DAM,
                    name: "LooseSoilDam",
                    density_kg_m3: 1650,
                    yield_strength_kpa: 7_500,
                    acid_susceptibility: 420,
                    electro_thermal_res: 350,
                    permeability: 420,
                    conductivity: 140,
                    cohesion: 220,
                    base_integrity: 3_200,
                    erosion_product: Some(MaterialId::SILT_CLAY),
                    debug_rgb: [118, 86, 56],
                },
                MaterialDef {
                    id: MaterialId::SILT_CLAY,
                    name: "SiltClay",
                    density_kg_m3: 1850,
                    yield_strength_kpa: 9_000,
                    acid_susceptibility: 260,
                    electro_thermal_res: 450,
                    permeability: 110,
                    conductivity: 220,
                    cohesion: 360,
                    base_integrity: 5_000,
                    erosion_product: None,
                    debug_rgb: [94, 78, 62],
                },
                MaterialDef {
                    id: MaterialId::MINERALIZED_BONE,
                    name: "MineralizedBone",
                    density_kg_m3: 2600,
                    yield_strength_kpa: 95_000,
                    acid_susceptibility: 110,
                    electro_thermal_res: 780,
                    permeability: 30,
                    conductivity: 540,
                    cohesion: 880,
                    base_integrity: 28_000,
                    erosion_product: Some(MaterialId::SILT_CLAY),
                    debug_rgb: [212, 206, 184],
                },
                MaterialDef {
                    id: MaterialId::CONDUCTIVE_ORE,
                    name: "ConductiveOre",
                    density_kg_m3: 3900,
                    yield_strength_kpa: 135_000,
                    acid_susceptibility: 70,
                    electro_thermal_res: 900,
                    permeability: 10,
                    conductivity: 860,
                    cohesion: 860,
                    base_integrity: 35_000,
                    erosion_product: Some(MaterialId::SANDSTONE),
                    debug_rgb: [164, 104, 62],
                },
                MaterialDef {
                    id: MaterialId::RUIN_ALLOY_WALL,
                    name: "RuinAlloyWall",
                    density_kg_m3: 7200,
                    yield_strength_kpa: 360_000,
                    acid_susceptibility: 18,
                    electro_thermal_res: 920,
                    permeability: 2,
                    conductivity: 920,
                    cohesion: 990,
                    base_integrity: 64_000,
                    erosion_product: Some(MaterialId::CONDUCTIVE_ORE),
                    debug_rgb: [92, 114, 128],
                },
                MaterialDef {
                    id: MaterialId::RUIN_CONDUIT_CASING,
                    name: "RuinConduitCasing",
                    density_kg_m3: 5400,
                    yield_strength_kpa: 210_000,
                    acid_susceptibility: 32,
                    electro_thermal_res: 850,
                    permeability: 8,
                    conductivity: 880,
                    cohesion: 930,
                    base_integrity: 48_000,
                    erosion_product: Some(MaterialId::CONDUCTIVE_ORE),
                    debug_rgb: [122, 146, 154],
                },
                MaterialDef {
                    id: MaterialId::RUIN_HEAT_CORE,
                    name: "RuinHeatCore",
                    density_kg_m3: 7800,
                    yield_strength_kpa: 420_000,
                    acid_susceptibility: 8,
                    electro_thermal_res: 990,
                    permeability: 1,
                    conductivity: 960,
                    cohesion: 995,
                    base_integrity: 65_000,
                    erosion_product: Some(MaterialId::RUIN_ALLOY_WALL),
                    debug_rgb: [216, 108, 56],
                },
            ],
        }
    }

    #[inline]
    pub const fn get(&self, id: MaterialId) -> &MaterialDef {
        let idx = if (id.0 as usize) < MaterialId::COUNT {
            id.0 as usize
        } else {
            0
        };
        &self.defs[idx]
    }
}

pub const MATERIALS: MaterialRegistry = MaterialRegistry::default_registry();
