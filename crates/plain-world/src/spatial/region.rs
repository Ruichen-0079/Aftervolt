use plain_contracts::telemetry::{EraPhase, WeatherSpectacle};
use plain_contracts::RegionId;
use serde::{Deserialize, Serialize};

/// Geographical role of an L3 MacroRegion / Basin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasinRole {
    RidgePlateau,
    CanyonTravertineTerrace,
    ElectrolyteSaltBasin,
    FaultBasin,
}

/// Compact per-macro-step net flux record for dormant chunk lazy catch-up (E.4 & Q7).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MacroFluxRecord<M = Option<f32>> {
    pub macro_step: u64,
    pub era: u8,
    pub weather: u8,
    /// Acid rain dissolution impulse (>0 dissolves SaltCrust & weakens soil dams).
    pub acid_erosion_impulse: f32,
    /// Arid evaporation salt crust deposition impulse (>0 deposits surface salt crust).
    pub salt_deposition_impulse: f32,
    /// Flood peak hydraulic shear (0.0..=1.0).
    pub flood_peak: f32,
    /// Net pH shift impulse applied to exposed/shallow L1 tiles.
    pub ph_delta: f32,
    /// Net fertility impulse (+ during red-rain/bloom recovery, - during drawdown).
    pub fertility_delta: f32,
    /// Net salinity impulse (+ during arid evaporation, - during freshwater flush).
    pub salinity_delta: f32,
    /// Ambient surface electrostatic forcing potential (Volts-equivalent).
    pub ambient_charge_v: f32,
    /// Net rainfall (+) or evaporation (-) micro-liters per surface cell for conservation sync.
    pub net_water_delta_per_col: i32,
    /// Net atmosphere-to-surface/pending transfer already settled by the macro water ledger.
    pub settled_water_ul: i64,
    /// Basin moisture during the replay interval. Legacy records have no sample.
    pub moisture: M,
}

/// Conserved totals and peak forcing for a contiguous run of evicted macro steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedFluxRecord<M = ArchivedMoisture> {
    pub start_step: u64,
    pub end_step: u64,
    pub era: u8,
    pub acid: f64,
    pub salt: f64,
    pub flood_peak: f32,
    pub flood_exposure: f64,
    pub ph: f64,
    pub fertility: f64,
    pub salinity: f64,
    pub charge: f64,
    pub water_per_col: i64,
    pub settled_water_ul: i64,
    pub moisture: M,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArchivedMoisture {
    pub sum: f64,
    pub samples: u64,
}

impl ArchivedMoisture {
    pub(crate) fn mean(&self) -> Option<f32> {
        (self.samples > 0).then(|| (self.sum / self.samples as f64) as f32)
    }

    pub(crate) fn add(&mut self, moisture: Option<f32>) {
        if let Some(moisture) = moisture {
            self.sum += f64::from(moisture);
            self.samples += 1;
        }
    }
}

impl ArchivedFluxRecord {
    fn append(&mut self, record: MacroFluxRecord) {
        self.end_step = record.macro_step;
        self.acid += f64::from(record.acid_erosion_impulse);
        self.salt += f64::from(record.salt_deposition_impulse);
        self.flood_peak = self.flood_peak.max(record.flood_peak);
        self.flood_exposure += f64::from(record.flood_peak);
        self.ph += f64::from(record.ph_delta);
        self.fertility += f64::from(record.fertility_delta);
        self.salinity += f64::from(record.salinity_delta);
        self.charge += f64::from(record.ambient_charge_v);
        self.water_per_col += i64::from(record.net_water_delta_per_col);
        self.settled_water_ul += record.settled_water_ul;
        self.moisture.add(record.moisture);
    }
}

/// Bounded detailed history with a compact archive of older, contiguous era runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegionalMacroFluxHistory<R = MacroFluxRecord, A = ArchivedFluxRecord> {
    pub records: Vec<R>,
    pub max_len: usize,
    pub archived_buckets: Vec<A>,
    /// Last step moved out of the detailed history.
    pub discarded_through: u64,
}

impl RegionalMacroFluxHistory {
    pub fn new(max_len: usize) -> Self {
        Self {
            records: Vec::with_capacity(max_len.min(4096)),
            max_len,
            archived_buckets: Vec::new(),
            discarded_through: 0,
        }
    }

    pub fn push(&mut self, rec: MacroFluxRecord) {
        self.push_preserving_cursors(rec, &[]);
    }

    pub(crate) fn push_preserving_cursors(&mut self, rec: MacroFluxRecord, cursors: &[u64]) {
        if self.max_len > 0 && self.records.len() >= self.max_len {
            let over_capacity = self.records.len() + 1 - self.max_len;
            let drop_n = (self.max_len / 4).max(over_capacity);
            for old in self.records.drain(0..drop_n) {
                let water = i64::from(old.net_water_delta_per_col);
                let append = self.archived_buckets.last().is_some_and(|bucket| {
                    bucket.end_step + 1 == old.macro_step
                        && bucket.era == old.era
                        && crate::fields::moisture_band(bucket.moisture.mean())
                            == crate::fields::moisture_band(old.moisture)
                        && (water == 0
                            || bucket.water_per_col == 0
                            || water.signum() == bucket.water_per_col.signum())
                        && cursors.binary_search(&bucket.end_step).is_err()
                });
                if !append {
                    self.archived_buckets.push(ArchivedFluxRecord {
                        start_step: old.macro_step,
                        end_step: old.macro_step,
                        era: old.era,
                        acid: 0.0,
                        salt: 0.0,
                        flood_peak: 0.0,
                        flood_exposure: 0.0,
                        ph: 0.0,
                        fertility: 0.0,
                        salinity: 0.0,
                        charge: 0.0,
                        water_per_col: 0,
                        settled_water_ul: 0,
                        moisture: ArchivedMoisture::default(),
                    });
                }
                self.archived_buckets.last_mut().unwrap().append(old);
                self.discarded_through = old.macro_step;
            }
        }
        self.records.push(rec);
    }

    pub fn slice_since(&self, last_step: u64) -> &[MacroFluxRecord] {
        let idx = self.records.partition_point(|r| r.macro_step <= last_step);
        &self.records[idx..]
    }
}

/// L3 MacroRegion / Basin owning conserved atmospheric reservoirs and 4-era hysteresis state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroRegion<H = RegionalMacroFluxHistory> {
    pub id: RegionId,
    pub name: String,
    pub role: BasinRole,
    /// Inclusive-exclusive horizontal span in L0 cells.
    pub x_start: usize,
    pub x_end: usize,
    /// Base reference surface elevation in L0 cells.
    pub mean_surface_y: u16,

    // Conserved L3 reservoirs (exact i64 accounting)
    pub water_vapor_ul: i64,
    /// Condensed water awaiting placement or re-evaporation (exact conservation).
    pub pending_surface_water_ul: i64,
    /// Rainwater eligible for the next basin-wide settlement.
    pub precipitation_remainder_ul: i64,
    /// Accumulated evaporation demand for the current day, in microliters.
    pub evaporation_remainder_ul: i64,
    pub mineral_dust_units: i64,
    pub surface_charge_uc: i64,

    // Normalized macroscopic state variables (0.0..=1.0) driving non-linear 4-era hysteresis
    pub moisture_index: f32,
    pub fertility_index: f32,
    pub salt_crust_ratio: f32,
    pub charge_index: f32,
    pub dust_saturation: f32,
    pub flower_sea_suitability: f32,

    pub era: EraPhase,
    pub weather: WeatherSpectacle,
    pub era_elapsed_macro_steps: u64,
    pub era_transitions_count: u32,

    pub flux_history: H,
}

impl<H> MacroRegion<H> {
    pub fn map_flux_history<T>(self, map: impl FnOnce(H) -> T) -> MacroRegion<T> {
        MacroRegion {
            id: self.id,
            name: self.name,
            role: self.role,
            x_start: self.x_start,
            x_end: self.x_end,
            mean_surface_y: self.mean_surface_y,
            water_vapor_ul: self.water_vapor_ul,
            pending_surface_water_ul: self.pending_surface_water_ul,
            precipitation_remainder_ul: self.precipitation_remainder_ul,
            evaporation_remainder_ul: self.evaporation_remainder_ul,
            mineral_dust_units: self.mineral_dust_units,
            surface_charge_uc: self.surface_charge_uc,
            moisture_index: self.moisture_index,
            fertility_index: self.fertility_index,
            salt_crust_ratio: self.salt_crust_ratio,
            charge_index: self.charge_index,
            dust_saturation: self.dust_saturation,
            flower_sea_suitability: self.flower_sea_suitability,
            era: self.era,
            weather: self.weather,
            era_elapsed_macro_steps: self.era_elapsed_macro_steps,
            era_transitions_count: self.era_transitions_count,
            flux_history: map(self.flux_history),
        }
    }
}
