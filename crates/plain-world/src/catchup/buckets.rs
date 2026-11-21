use crate::fields::moisture_band;
use crate::spatial::{
    ArchivedFluxRecord, ArchivedMoisture, MacroFluxRecord, RegionalMacroFluxHistory,
};

#[derive(Default)]
pub(super) struct FluxBucket {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) era: u8,
    archived: bool,
    pub(super) acid: f64,
    pub(super) salt: f64,
    pub(super) flood_exposure: f64,
    pub(super) ph: f64,
    pub(super) fertility: f64,
    pub(super) salinity: f64,
    pub(super) water: i64,
    pub(super) moisture: ArchivedMoisture,
}

impl FluxBucket {
    fn add(&mut self, record: &MacroFluxRecord) {
        self.end = record.macro_step;
        self.acid += f64::from(record.acid_erosion_impulse);
        self.salt += f64::from(record.salt_deposition_impulse);
        self.flood_exposure += f64::from(record.flood_peak);
        self.ph += f64::from(record.ph_delta);
        self.fertility += f64::from(record.fertility_delta);
        self.salinity += f64::from(record.salinity_delta);
        self.water += i64::from(record.net_water_delta_per_col);
        self.moisture.add(record.moisture);
    }

    fn from_archive(record: &ArchivedFluxRecord, start: u64, end: u64) -> Self {
        let start = start.max(record.start_step);
        let end = end.min(record.end_step);
        let len = record.end_step - record.start_step + 1;
        let fraction = (end - start + 1) as f64 / len as f64;
        // Normal archival seals runs at extant chunk cursors. A replay clipped by
        // incomplete publication prorates the totals and partitions integer water.
        let water_prefix =
            |offset| i128::from(record.water_per_col) * i128::from(offset) / i128::from(len);
        Self {
            start,
            end,
            era: record.era,
            archived: true,
            acid: record.acid * fraction,
            salt: record.salt * fraction,
            flood_exposure: record.flood_exposure * fraction,
            ph: record.ph * fraction,
            fertility: record.fertility * fraction,
            salinity: record.salinity * fraction,
            moisture: record.moisture.clone(),
            water: (water_prefix(end - record.start_step + 1)
                - water_prefix(start - record.start_step)) as i64,
        }
    }
}

pub(super) fn collect_buckets(
    history: &RegionalMacroFluxHistory,
    last_step: u64,
    realized_step: u64,
) -> Vec<FluxBucket> {
    let mut buckets = Vec::new();
    let archive_start = history
        .archived_buckets
        .partition_point(|b| b.end_step <= last_step);
    for record in &history.archived_buckets[archive_start..] {
        if record.start_step > realized_step {
            break;
        }
        buckets.push(FluxBucket::from_archive(
            record,
            last_step + 1,
            realized_step,
        ));
    }
    for record in history
        .slice_since(last_step)
        .iter()
        .take_while(|r| r.macro_step <= realized_step)
    {
        if buckets.last().is_none_or(|bucket| {
            bucket.archived
                || bucket.era != record.era
                || moisture_band(bucket.moisture.mean()) != moisture_band(record.moisture)
                || (record.net_water_delta_per_col != 0
                    && bucket.water != 0
                    && i64::from(record.net_water_delta_per_col).signum() != bucket.water.signum())
        }) {
            buckets.push(FluxBucket {
                start: record.macro_step,
                end: record.macro_step,
                era: record.era,
                ..FluxBucket::default()
            });
        }
        buckets.last_mut().unwrap().add(record);
    }
    buckets
}

pub(super) fn published_through(history: &RegionalMacroFluxHistory, last: u64, target: u64) -> u64 {
    let mut end = last;
    let start = history
        .archived_buckets
        .partition_point(|bucket| bucket.end_step <= last);
    for archived in &history.archived_buckets[start..] {
        if archived.start_step > end + 1 {
            return end;
        }
        end = archived.end_step.min(target);
        if end == target {
            return end;
        }
    }
    let records = history.slice_since(end);
    let records = &records[..records.partition_point(|r| r.macro_step <= target)];
    if let Some(last) = records.last() {
        if last.macro_step - end == records.len() as u64 {
            return last.macro_step;
        }
    }
    for record in records {
        if record.macro_step != end + 1 {
            break;
        }
        end = record.macro_step;
    }
    end
}
