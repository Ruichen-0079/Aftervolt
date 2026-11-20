use serde::{Deserialize, Serialize};

/// Maximum fluid units stored in a single L0 cell (10,000 = 100.00% full).
/// Exact integer transfers guarantee strict world-wide mass/water conservation (F6).
pub const CELL_MAX_FLUID_UNITS: u16 = 10_000;

/// Conserved fluid volume in internal integer micro-liters (or scaled volume units).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct MicroLiters(pub i64);

/// Conserved chemical energy in millijoules (F6: dual-track energy, never merged with electric).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct MilliJoules(pub i64);

/// Conserved electric charge reserve in microcoulombs (F6: dual-track energy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct MicroCoulombs(pub i64);

/// Scaled pH value: 0..=140 maps linearly to pH 0.0..=14.0 (70 = neutral 7.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PhScaled(pub u8);

impl PhScaled {
    pub const NEUTRAL: Self = Self(70);

    #[inline]
    pub const fn new(raw: u8) -> Self {
        if raw > 140 {
            Self(140)
        } else {
            Self(raw)
        }
    }

    #[inline]
    pub fn as_f32(self) -> f32 {
        (self.0 as f32) * 0.1
    }
}

/// Temperature in 0.1 Kelvin (e.g., 2932 = 293.2 K = 20.05 C).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct DeciKelvin(pub u16);

impl DeciKelvin {
    pub const ZERO_C: Self = Self(2732);
    pub const AMBIENT_20C: Self = Self(2932);

    #[inline]
    pub fn as_celsius(self) -> f32 {
        (self.0 as f32 - 2731.5) * 0.1
    }

    #[inline]
    pub fn from_celsius(c: f32) -> Self {
        let dk = (c * 10.0 + 2731.5).clamp(100.0, 60000.0);
        Self(dk as u16)
    }
}

/// Salinity concentration in scaled parts-per-million / units (0..=65535).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct SalinityPpm(pub u16);

/// Soil fertility & bioavailable electrolyte density (0..=65535).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct FertilityUnits(pub u16);
