use plain_contracts::materials::{FluidKind, MaterialId, MATERIALS};
use plain_contracts::telemetry::{
    CellProbeReport, ConservationLedger, DevTelemetryPort, OverlayKind, ProfilerSnapshot,
    RegionTelemetry, ViewportFieldSnapshot, ViewportRect, WorldOverviewSnapshot,
};
use plain_contracts::{CellCoord, CHUNK_SIZE};
use rayon::prelude::*;

use crate::electric::sample_electric_gradient;
use crate::scheduler::WorldSimulation;
use crate::spatial::{WorldState, CELL_FLAG_DORMANT_BANK, CELL_FLAG_RUIN_ANCHOR};

impl DevTelemetryPort for WorldSimulation {
    fn render_world_overview(
        &self,
        target_w: u32,
        target_h: u32,
        overlay: OverlayKind,
    ) -> WorldOverviewSnapshot {
        let full = ViewportRect {
            min_x: 0,
            min_y: 0,
            max_x: self.world.width_cells() as i32 - 1,
            max_y: self.world.height_cells() as i32 - 1,
        };
        let vp = self.render_viewport(full, target_w, target_h, overlay);
        WorldOverviewSnapshot {
            width_px: vp.width_px,
            height_px: vp.height_px,
            rgba: vp.rgba,
        }
    }

    fn render_viewport(
        &self,
        viewport: ViewportRect,
        target_w: u32,
        target_h: u32,
        overlay: OverlayKind,
    ) -> ViewportFieldSnapshot {
        let w = target_w.max(1) as usize;
        let h = target_h.max(1) as usize;
        let mut rgba = vec![0u8; w * h * 4];

        let span_x = (viewport.max_x - viewport.min_x).max(1) as f64;
        let span_y = (viewport.max_y - viewport.min_y).max(1) as f64;
        let world = &self.world;
        let active_mask = self.active_mask();

        // Render rows in parallel for silky real-time 60 FPS developer inspection.
        rgba.par_chunks_mut(w * 4).enumerate().for_each(|(py, row)| {
            // Flip Y so screen top = high altitude (world +Y up).
            let norm_y = 1.0 - (py as f64 + 0.5) / (h as f64);
            let wy = (viewport.min_y as f64 + norm_y * span_y) as i32;

            for px in 0..w {
                let norm_x = (px as f64 + 0.5) / (w as f64);
                let wx = (viewport.min_x as f64 + norm_x * span_x) as i32;
                let cell = CellCoord::new(wx, wy);
                let color = sample_overlay_rgba(world, active_mask, cell, overlay);
                let base = px * 4;
                row[base] = color[0];
                row[base + 1] = color[1];
                row[base + 2] = color[2];
                row[base + 3] = 255;
            }
        });

        ViewportFieldSnapshot {
            width_px: w as u32,
            height_px: h as u32,
            overlay,
            viewport,
            rgba,
        }
    }

    fn probe_cell(&self, cell: CellCoord) -> Option<CellProbeReport> {
        if !self.world.config.dimensions.contains_cell(cell) {
            return None;
        }
        let cc = cell.chunk_coord();
        let ci = self.world.chunk_index(cc)?;
        let li = cell.local_index();
        let ch = &self.world.l0_chunks[ci];
        let mat = MaterialId(ch.solid_material[li]);
        let def = MATERIALS.get(mat);
        let fkind = FluidKind::from_u8(ch.fluid_kind[li]);
        let fvol = ch.fluid_volume[li];

        let (ph, fertility, salinity, temperature, microbes, e_pot, _cond, flux_mask) =
            self.world.sample_l1_at_cell(cell)?;
        let world_pos = cell.to_world_meters_center(self.world.config.meters_per_cell);
        let esample = sample_electric_gradient(&self.world, world_pos, false);

        let rid = self.world.region_for_x(cell.x as usize);
        let region = &self.world.regions[rid.0 as usize];

        Some(CellProbeReport {
            world_pos,
            cell,
            region_id: rid,
            material: mat,
            material_name: def.name.to_string(),
            yield_strength_kpa: def.yield_strength_kpa,
            acid_susceptibility: def.acid_susceptibility,
            integrity: ch.solid_integrity[li],
            fluid_kind: fkind,
            fluid_volume: fvol,
            ph,
            fertility,
            salinity,
            temperature,
            microbe_saprotroph: microbes[0],
            microbe_conductive: microbes[1],
            microbe_halophile: microbes[2],
            microbe_ferro: microbes[3],
            electric_potential: e_pot,
            electric_grad_x: esample.grad_x,
            electric_grad_y: esample.grad_y,
            flux_block_mask: flux_mask,
            chunk_sleeping: ch.sleep.fluid_sleeping,
            chunk_last_macro_step: ch.last_realized_macro_step,
            era: region.era,
            weather: region.weather,
        })
    }

    fn conservation_ledger(&self) -> ConservationLedger {
        let (surface_w, vapor_w, total_w) = self.world.total_world_water();
        let mut active_chunks = 0u32;
        let mut sleeping_chunks = 0u32;
        let mut dirty_save_chunks = 0u32;
        let mut salt_cells = 0u64;

        for ch in &self.world.l0_chunks {
            if ch.sleep.fluid_sleeping {
                sleeping_chunks += 1;
            } else {
                active_chunks += 1;
            }
            if ch.sleep.dirty_for_save {
                dirty_save_chunks += 1;
            }
            for &m in &ch.solid_material {
                if m == MaterialId::SALT_CRUST.0 {
                    salt_cells += 1;
                }
            }
        }

        let mut total_fert = 0u64;
        let mut total_sal = 0u64;
        for f in &self.world.l1_chunks {
            for &v in &f.fertility {
                total_fert += v as u64;
            }
            for &v in &f.salinity {
                total_sal += v as u64;
            }
        }

        let chunks_x = self.world.config.dimensions.chunks_x as usize;
        let steps_per_day = self.world.config.clock.macro_steps_per_game_day.max(1) as f32;

        let regions = self
            .world
            .regions
            .iter()
            .map(|r| {
                // Sample L1 average pH and fertility across this region's chunks
                let c0 = r.x_start / CHUNK_SIZE;
                let c1 = (r.x_end.saturating_sub(1) / CHUNK_SIZE).min(chunks_x - 1);
                let mut ph_sum = 0u64;
                let mut f_sum = 0u64;
                let mut cnt = 0u64;
                for cy in 0..(self.world.config.dimensions.chunks_y as usize) {
                    for cx in c0..=c1 {
                        let l1 = &self.world.l1_chunks[cy * chunks_x + cx];
                        for i in 0..l1.ph_milli.len() {
                            ph_sum += l1.ph_milli[i] as u64;
                            f_sum += l1.fertility[i] as u64;
                            cnt += 1;
                        }
                    }
                }
                let avg_ph = if cnt > 0 {
                    (ph_sum as f32 / cnt as f32) * 0.001
                } else {
                    7.0
                };
                let avg_fert = if cnt > 0 {
                    (f_sum as f32 / cnt as f32) / 65535.0
                } else {
                    0.0
                };

                RegionTelemetry {
                    id: r.id,
                    name: r.name.clone(),
                    x_start: r.x_start,
                    x_end: r.x_end,
                    era: r.era,
                    weather: r.weather,
                    era_age_days: (r.era_elapsed_macro_steps as f32) / steps_per_day,
                    water_vapor: plain_contracts::MicroLiters(r.water_vapor_ul),
                    mineral_dust: r.mineral_dust_units,
                    surface_charge: plain_contracts::MicroCoulombs(r.surface_charge_uc),
                    moisture_index: r.moisture_index,
                    salt_crust_ratio: r.salt_crust_ratio,
                    flower_sea_suitability: r.flower_sea_suitability,
                    avg_ph,
                    avg_fertility: avg_fert,
                }
            })
            .collect();

        ConservationLedger {
            tick: self.world.clock.tick,
            macro_step: self.world.clock.macro_step,
            game_day: self.world.clock.current_game_day(),
            total_surface_fluid: surface_w,
            total_atmospheric_vapor: vapor_w,
            total_world_water: total_w,
            initial_world_water: plain_contracts::MicroLiters(self.world.initial_world_water_ul),
            total_fertility_sum: total_fert,
            total_salinity_sum: total_sal,
            total_atmospheric_dust: self.world.regions.iter().map(|r| r.mineral_dust_units).sum(),
            total_surface_charge: self.world.total_charge_sum(),
            total_salt_crust_cells: salt_cells,
            active_chunks,
            sleeping_chunks,
            dirty_save_chunks,
            regions,
        }
    }

    fn profiler_snapshot(&self) -> ProfilerSnapshot {
        self.build_profiler_snapshot()
    }
}

#[inline]
fn sample_overlay_rgba(
    world: &WorldState,
    active_mask: &[bool],
    cell: CellCoord,
    overlay: OverlayKind,
) -> [u8; 3] {
    if !world.config.dimensions.contains_cell(cell) {
        return [8, 10, 14];
    }
    let cc = cell.chunk_coord();
    let Some(ci) = world.chunk_index(cc) else {
        return [8, 10, 14];
    };
    let li = cell.local_index();
    let ch = &world.l0_chunks[ci];
    let mat = MaterialId(ch.solid_material[li]);
    let fkind = FluidKind::from_u8(ch.fluid_kind[li]);
    let fvol = ch.fluid_volume[li];
    let flags = ch.cell_flags[li];

    match overlay {
        OverlayKind::CompositeTerrainFluid => {
            if mat.is_solid() {
                let mut rgb = MATERIALS.get(mat).debug_rgb;
                if (flags & CELL_FLAG_DORMANT_BANK) != 0 && mat != MaterialId::SALT_CRUST {
                    // Subtle warm ochre tint indicating dormant desert seed/cocoon bank
                    rgb[0] = rgb[0].saturating_add(16);
                    rgb[1] = rgb[1].saturating_add(14);
                }
                if (flags & CELL_FLAG_RUIN_ANCHOR) != 0 && !mat.is_ruin_material() {
                    rgb[2] = rgb[2].saturating_add(18);
                }
                rgb
            } else if fvol > 0 {
                let alpha = (0.35 + 0.65 * (fvol as f32 / 10_000.0)).min(1.0);
                let base = match fkind {
                    FluidKind::RainPool => [72, 168, 224],
                    FluidKind::TravertinePool => [84, 212, 198],
                    FluidKind::SaltMarsh => [118, 214, 178],
                    FluidKind::ThunderRiver => [98, 152, 244],
                    FluidKind::BlackWater => [22, 30, 44],
                    FluidKind::None => [20, 26, 36],
                };
                [
                    (base[0] as f32 * alpha) as u8,
                    (base[1] as f32 * alpha) as u8,
                    (base[2] as f32 * alpha) as u8,
                ]
            } else {
                // Sky tinted gently by regional era atmosphere
                let rid = world.region_for_x(cell.x as usize).0 as usize;
                let era_tint = world
                    .regions
                    .get(rid)
                    .map(|r| r.era.debug_rgb())
                    .unwrap_or([24, 30, 40]);
                let norm_y = (cell.y as f32 / world.height_cells().max(1) as f32).clamp(0.0, 1.0);
                let k = if norm_y > 0.45 { 0.16 } else { 0.06 };
                [
                    (14.0 + era_tint[0] as f32 * k) as u8,
                    (18.0 + era_tint[1] as f32 * k) as u8,
                    (24.0 + era_tint[2] as f32 * k) as u8,
                ]
            }
        }
        OverlayKind::MaterialYieldStrength => {
            if mat.is_air() {
                return [12, 14, 18];
            }
            let norm = (MATERIALS.get(mat).yield_strength_kpa as f32 / 360_000.0).clamp(0.0, 1.0);
            heat_ramp_inferno(norm)
        }
        OverlayKind::PhField => {
            let Some((ph, _, _, _, _, _, _, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            // Red = acidic (pH 3), Green = neutral (pH 7), Blue/Purple = alkaline (pH 11)
            let t = ((ph.0 as f32 - 35.0) / 75.0).clamp(0.0, 1.0);
            let mut rgb = if t < 0.5 {
                let u = t * 2.0;
                [(235.0 * (1.0 - u) + 60.0 * u) as u8, (60.0 + 165.0 * u) as u8, (50.0 + 40.0 * u) as u8]
            } else {
                let u = (t - 0.5) * 2.0;
                [(60.0 + 95.0 * u) as u8, (225.0 * (1.0 - u) + 75.0 * u) as u8, (90.0 + 155.0 * u) as u8]
            };
            if mat.is_solid() {
                rgb = [rgb[0] / 2, rgb[1] / 2, rgb[2] / 2];
            }
            rgb
        }
        OverlayKind::FertilityField => {
            let Some((_, fert, _, _, _, _, _, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            let t = fert.0 as f32 / 65_535.0;
            let base = [(40.0 + 45.0 * (1.0 - t)) as u8, (35.0 + 210.0 * t) as u8, (30.0 + 60.0 * t) as u8];
            if mat.is_air() && fvol == 0 { [base[0] / 3, base[1] / 3, base[2] / 3] } else { base }
        }
        OverlayKind::SalinityField => {
            let Some((_, _, sal, _, _, _, _, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            let t = sal.0 as f32 / 65_535.0;
            let v = (30.0 + 225.0 * t) as u8;
            [v, v, (25.0 + 195.0 * t) as u8]
        }
        OverlayKind::TemperatureField => {
            let Some((_, _, _, temp, _, _, _, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            let t = ((temp.0 as f32 - 2750.0) / 650.0).clamp(0.0, 1.0);
            heat_ramp_inferno(t)
        }
        OverlayKind::MicrobeSaprotroph => {
            let Some((_, _, _, _, m, _, _, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            let t = m[0] as f32 / 255.0;
            [(30.0 + 190.0 * t) as u8, (25.0 + 145.0 * t) as u8, (20.0 + 40.0 * t) as u8]
        }
        OverlayKind::MicrobeConductive => {
            let Some((_, _, _, _, m, _, _, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            let t = m[1] as f32 / 255.0;
            [(24.0 + 200.0 * t) as u8, (22.0 + 100.0 * t) as u8, (25.0 + 45.0 * t) as u8]
        }
        OverlayKind::MicrobeHalophile => {
            let Some((_, _, _, _, m, _, _, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            let t = m[2] as f32 / 255.0;
            [(20.0 + 55.0 * t) as u8, (25.0 + 205.0 * t) as u8, (35.0 + 195.0 * t) as u8]
        }
        OverlayKind::MicrobeFerro => {
            let Some((_, _, _, _, m, _, _, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            let t = m[3] as f32 / 255.0;
            [(30.0 + 185.0 * t) as u8, (20.0 + 65.0 * t) as u8, (30.0 + 135.0 * t) as u8]
        }
        OverlayKind::ElectricPotential => {
            let Some((_, _, _, _, _, v, cond, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            let t = ((v + 40.0) / 280.0).clamp(0.0, 1.0);
            let c = (cond as f32 / 1200.0).clamp(0.0, 1.0);
            [
                (20.0 + 180.0 * t) as u8,
                (25.0 + 140.0 * c + 70.0 * t) as u8,
                (40.0 + 215.0 * t.max(c * 0.7)) as u8,
            ]
        }
        OverlayKind::ChunkActivityDebug => {
            let is_border = cell.x.rem_euclid(CHUNK_SIZE as i32) == 0
                || cell.y.rem_euclid(CHUNK_SIZE as i32) == 0;
            if is_border {
                return [55, 60, 72];
            }
            let active = active_mask[ci];
            let awake = !ch.sleep.fluid_sleeping;
            let dirty = ch.sleep.dirty_for_save;
            match (active, awake, dirty) {
                (true, true, _) => [64, 210, 120],
                (true, false, _) => [52, 130, 196],
                (false, true, _) => [214, 168, 56],
                (false, false, true) => [110, 68, 96],
                (false, false, false) => [24, 28, 36],
            }
        }
        OverlayKind::BiomeSuitability => {
            let Some((ph, fert, sal, _, _, _, _, _)) = world.sample_l1_at_cell(cell) else {
                return [0, 0, 0];
            };
            if mat.is_air() && fvol == 0 {
                return [14, 18, 26];
            }
            // Highlight Electrolyte Flower Sea suitability (rich fertility + moderate/high salinity + moderate pH)
            let f_score = (fert.0 as f32 / 40_000.0).clamp(0.0, 1.0);
            let s_score = (sal.0 as f32 / 28_000.0).clamp(0.0, 1.0);
            let ph_score = (1.0 - (ph.0 as f32 - 70.0).abs() / 35.0).clamp(0.0, 1.0);
            let flower_suit = f_score * s_score * ph_score;
            if flower_suit > 0.25 {
                [
                    (40.0 + 60.0 * flower_suit) as u8,
                    (110.0 + 145.0 * flower_suit) as u8,
                    (120.0 + 125.0 * flower_suit) as u8,
                ]
            } else {
                let rgb = MATERIALS.get(mat).debug_rgb;
                [rgb[0] / 2, rgb[1] / 2, rgb[2] / 2]
            }
        }
    }
}

#[inline]
fn heat_ramp_inferno(t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    if t < 0.33 {
        let u = t * 3.0;
        [(20.0 + 110.0 * u) as u8, (15.0 + 25.0 * u) as u8, (45.0 + 95.0 * u) as u8]
    } else if t < 0.66 {
        let u = (t - 0.33) * 3.0;
        [(130.0 + 105.0 * u) as u8, (40.0 + 90.0 * u) as u8, (140.0 * (1.0 - u) + 30.0 * u) as u8]
    } else {
        let u = (t - 0.66) * 3.0;
        [(235.0 + 20.0 * u) as u8, (130.0 + 120.0 * u) as u8, (30.0 + 160.0 * u) as u8]
    }
}
