#![deny(clippy::todo, clippy::unimplemented)]

//! Frozen foundational contracts for Electroruin Plain (`Rust World Core`).
//!
//! Covers contracts F2, F3, F4, F5, F6, F7 and the 1220-game-day four-era cycle configuration.
//! Pure domain types and deterministic utilities with zero game-engine dependencies.

pub mod config;
pub mod coords;
pub mod materials;
pub mod rng;
pub mod telemetry;
pub mod time;
pub mod units;

pub use config::{FourEraCycleConfig, WorldConfig};
pub use coords::{
    CellCoord, ChunkCoord, FieldCoord, FieldDownsample, GroupId, PersistentUid, RegionId,
    SpeciesId, WorldDimensions, WorldPosMeters, CHUNK_CELLS, CHUNK_SIZE,
};
pub use materials::{FluidKind, MaterialDef, MaterialId, MaterialRegistry};
pub use rng::{det_cos, det_exp, det_floor, det_pow, det_sin, det_sqrt, MasterSeed};
pub use telemetry::{
    CellProbeReport, ConservationLedger, EraPhase, ObserverCommand, OverlayKind,
    PlayerPerceptionPort, ProfilerSnapshot, RegionTelemetry, SubsystemTimingsUs, ViewportRect,
    WeatherSpectacle, WorldOverviewSnapshot, ViewportFieldSnapshot,
};
#[cfg(feature = "dev-telemetry")]
pub use telemetry::DevTelemetryPort;
pub use time::{ClockConfig, GameDay, MacroStep, MultiRateClock, Tick};
pub use units::{
    DeciKelvin, FertilityUnits, MicroCoulombs, MicroLiters, MilliJoules, PhScaled, SalinityPpm,
    CELL_MAX_FLUID_UNITS,
};
