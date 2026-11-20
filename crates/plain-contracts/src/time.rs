use serde::{Deserialize, Serialize};

/// Monotonic fixed-step fine simulation tick (F3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Tick(pub u64);

/// Monotonic macro climate & geological step (E.4 multi-rate clock for 1220-day cycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct MacroStep(pub u64);

/// Elapsed game days since world genesis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct GameDay(pub u64);

/// Multi-rate clock configuration (F3 & Q2/Q7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockConfig {
    /// Fine physics ticks per real second at 1x speed (candidate: 60 or 30, Q2).
    pub fine_hz: u32,
    /// Real seconds per game day at 1x speed (default 900 = 15 real min/day).
    pub real_seconds_per_game_day: u32,
    /// Fine ticks between meso environment/microbial updates in active zones.
    pub meso_period_ticks: u32,
    /// Macro climate steps per game day (candidate: 24 = 1/hr or 4 = 6hr, Q7).
    pub macro_steps_per_game_day: u32,
    /// Fine ticks between local L1 electric field relaxation passes in active zones.
    pub electric_period_ticks: u32,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            fine_hz: 60,
            real_seconds_per_game_day: 900,
            meso_period_ticks: 60,
            macro_steps_per_game_day: 24,
            electric_period_ticks: 4, // 15 Hz at 60 Hz fine_hz
        }
    }
}

impl ClockConfig {
    #[inline]
    pub const fn ticks_per_game_day(self) -> u64 {
        (self.fine_hz as u64) * (self.real_seconds_per_game_day as u64)
    }

    #[inline]
    pub const fn ticks_per_macro_step(self) -> u64 {
        let denom = if self.macro_steps_per_game_day == 0 {
            1
        } else {
            self.macro_steps_per_game_day as u64
        };
        let t = self.ticks_per_game_day() / denom;
        if t == 0 {
            1
        } else {
            t
        }
    }

    #[inline]
    pub fn dt_fine_secs(self) -> f64 {
        1.0 / (self.fine_hz.max(1) as f64)
    }
}

/// Authoritative world multi-rate clock state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiRateClock {
    pub tick: Tick,
    pub macro_step: MacroStep,
    pub config: ClockConfig,
}

impl MultiRateClock {
    pub const fn new(config: ClockConfig) -> Self {
        Self {
            tick: Tick(0),
            macro_step: MacroStep(0),
            config,
        }
    }

    /// Advance by 1 fine tick. Returns `(fire_electric, fire_meso, fire_macro)`.
    #[inline]
    pub fn advance_fine_tick(&mut self) -> (bool, bool, bool) {
        self.tick.0 += 1;
        let t = self.tick.0;
        let fire_electric = t % (self.config.electric_period_ticks.max(1) as u64) == 0;
        let fire_meso = t % (self.config.meso_period_ticks.max(1) as u64) == 0;
        let tpm = self.config.ticks_per_macro_step();
        let fire_macro = t % tpm == 0;
        if fire_macro {
            self.macro_step.0 += 1;
        }
        (fire_electric, fire_meso, fire_macro)
    }

    /// Advance directly by `steps` macro steps without stepping intermediate fine ticks (E.4).
    #[inline]
    pub fn advance_macro_steps(&mut self, steps: u64) {
        self.macro_step.0 += steps;
        self.tick.0 += steps * self.config.ticks_per_macro_step();
    }

    #[inline]
    pub const fn current_game_day(&self) -> GameDay {
        let denom = if self.config.macro_steps_per_game_day == 0 {
            1
        } else {
            self.config.macro_steps_per_game_day as u64
        };
        GameDay(self.macro_step.0 / denom)
    }

    #[inline]
    pub fn fractional_game_day(&self) -> f64 {
        (self.tick.0 as f64) / (self.config.ticks_per_game_day().max(1) as f64)
    }
}
