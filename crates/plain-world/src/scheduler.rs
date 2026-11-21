use std::collections::VecDeque;
use std::time::Instant;

use plain_contracts::telemetry::{
    ObserverCommand, ProfilerSnapshot, SubsystemTimingsUs, ViewportRect,
};
use plain_contracts::{ChunkCoord, CHUNK_SIZE};

use crate::catchup::{catch_up_active_chunks, catch_up_fine_tick, catch_up_world};
use crate::climate::step_macro_climate;
use crate::electric::{solve_local_electric_field, viewport_chunk_bounds};
use crate::fields::step_scheduled_fields;
use crate::fluid::step_fluid_active_chunks;
use crate::spatial::WorldState;

/// Pure read-only developer telemetry camera (F7/F9: zero physical presence, never emits signals).
#[derive(Debug, Clone, Copy)]
pub struct ObserverCamera {
    pub id: u8,
    pub viewport: ViewportRect,
}

/// Rolling microsecond profiler tracking P50/P95/P99 frame budgets (F9).
#[derive(Debug, Clone)]
pub struct ProfilerState {
    pub latest: SubsystemTimingsUs,
    history_total_us: VecDeque<u64>,
    pub fine_ticks_executed: u64,
    pub macro_steps_executed: u64,
}

impl Default for ProfilerState {
    fn default() -> Self {
        Self {
            latest: SubsystemTimingsUs::default(),
            history_total_us: VecDeque::with_capacity(256),
            fine_ticks_executed: 0,
            macro_steps_executed: 0,
        }
    }
}

impl ProfilerState {
    pub fn record(&mut self, sample: SubsystemTimingsUs) {
        self.latest = sample;
        if self.history_total_us.len() >= 256 {
            self.history_total_us.pop_front();
        }
        self.history_total_us.push_back(sample.total_us);
    }

    pub fn percentiles(&self) -> (u64, u64, u64) {
        if self.history_total_us.is_empty() {
            return (0, 0, 0);
        }
        let mut sorted: Vec<u64> = self.history_total_us.iter().copied().collect();
        sorted.sort_unstable();
        let n = sorted.len();
        let p50 = sorted[n / 2];
        let p95 = sorted[((n * 95) / 100).min(n - 1)];
        let p99 = sorted[((n * 99) / 100).min(n - 1)];
        (p50, p95, p99)
    }
}

/// Top-level simulation runtime combining authoritative `WorldState`, multi-camera observer set,
/// multi-rate scheduler (Fine/Meso/Macro), and built-in subsystem profiler.
pub struct WorldSimulation {
    pub world: WorldState,
    pub dev_cameras: Vec<ObserverCamera>,
    simulation_anchors: Vec<(u8, ViewportRect)>,
    ready_active_mask: Vec<bool>,
    pub profiler: ProfilerState,
    /// Reusable bitmask marking LOD-0 active chunks across the union of all simulation anchors.
    active_chunk_mask: Vec<bool>,
    pub active_lod0_count: u32,
}

impl WorldSimulation {
    pub fn new(world: WorldState) -> Self {
        let total = world.config.dimensions.total_chunks();
        let mut sim = Self {
            world,
            dev_cameras: Vec::new(),
            simulation_anchors: Vec::new(),
            ready_active_mask: vec![false; total],
            profiler: ProfilerState::default(),
            active_chunk_mask: vec![false; total],
            active_lod0_count: 0,
        };
        // Default simulation anchor centered on the middle basin at surface height
        let w = sim.world.width_cells() as i32;
        let mid_x = w / 2;
        let mid_y = sim
            .world
            .surface_reference_height
            .get(mid_x as usize)
            .or_else(|| sim.world.topology.surface_height.get(mid_x as usize))
            .copied()
            .unwrap_or((sim.world.height_cells() / 2) as u16) as i32;
        sim.set_simulation_anchor(
            0,
            ViewportRect {
                min_x: (mid_x - 256).max(0),
                min_y: (mid_y - 160).max(0),
                max_x: (mid_x + 256).min(w - 1),
                max_y: (mid_y + 160).min(sim.world.height_cells() as i32 - 1),
            },
        );
        sim
    }

    pub fn set_dev_camera(&mut self, id: u8, viewport: ViewportRect) {
        if let Some(cam) = self.dev_cameras.iter_mut().find(|c| c.id == id) {
            cam.viewport = viewport;
        } else {
            self.dev_cameras.push(ObserverCamera { id, viewport });
        }
    }

    pub fn remove_dev_camera(&mut self, id: u8) {
        self.dev_cameras.retain(|c| c.id != id);
    }

    pub fn set_simulation_anchor(&mut self, id: u8, viewport: ViewportRect) {
        if let Some((_, current)) = self
            .simulation_anchors
            .iter_mut()
            .find(|(key, _)| *key == id)
        {
            *current = viewport;
        } else {
            self.simulation_anchors.push((id, viewport));
        }
        self.rebuild_active_mask();
    }

    pub fn remove_simulation_anchor(&mut self, id: u8) {
        self.simulation_anchors.retain(|&(key, _)| key != id);
        self.rebuild_active_mask();
    }

    pub fn apply_command(&mut self, cmd: ObserverCommand) {
        match cmd {
            ObserverCommand::SetDevCamera { id, viewport } => self.set_dev_camera(id, viewport),
            ObserverCommand::RemoveDevCamera { id } => self.remove_dev_camera(id),
            ObserverCommand::StepFineTicks { count } => {
                for _ in 0..count {
                    self.step_fine_tick();
                }
            }
            ObserverCommand::StepMacroSteps { count } => {
                self.step_macro_steps(count, true);
            }
        }
    }

    pub fn rebuild_active_mask(&mut self) {
        self.active_chunk_mask.fill(false);
        let pad = self.world.config.active_lod0_padding_chunks;
        let mut count = 0u32;

        for &(_, viewport) in &self.simulation_anchors {
            let (min_c, max_c) = viewport_chunk_bounds(
                &self.world,
                viewport.min_x,
                viewport.min_y,
                viewport.max_x,
                viewport.max_y,
                pad,
            );
            for cy in min_c.cy..=max_c.cy {
                for cx in min_c.cx..=max_c.cx {
                    if let Some(ci) = self.world.chunk_index(ChunkCoord::new(cx, cy)) {
                        if !self.active_chunk_mask[ci] {
                            self.active_chunk_mask[ci] = true;
                            count += 1;
                        }
                    }
                }
            }
        }
        self.active_lod0_count = count;
    }

    fn refresh_ready_mask(&mut self) {
        let step = self.world.clock.macro_step.0;
        for ((ready, &active), chunk) in self
            .ready_active_mask
            .iter_mut()
            .zip(&self.active_chunk_mask)
            .zip(&self.world.l0_chunks)
        {
            *ready = active && chunk.last_realized_macro_step == step;
        }
    }

    /// Force all chunks active (used in Q3 full-world worst-case stress benchmarks).
    pub fn set_all_chunks_active(&mut self) {
        self.active_chunk_mask.fill(true);
        self.active_lod0_count = self.active_chunk_mask.len() as u32;
    }

    /// Execute one fixed fine tick (`1/fine_hz`), triggering electric, meso, and macro steps on cadence.
    /// Returns `true` if any basin changed `EraPhase` during this tick.
    pub fn step_fine_tick(&mut self) -> bool {
        let t_total = Instant::now();
        let mut timings = SubsystemTimingsUs::default();

        let (fire_electric, fire_meso, fire_macro) = self.world.clock.advance_fine_tick();
        self.profiler.fine_ticks_executed += 1;

        // Publish this macro step before any active chunk can realize it.
        let mut era_changed = false;
        if fire_macro {
            let t_macro = Instant::now();
            era_changed = step_macro_climate(&mut self.world);
            self.profiler.macro_steps_executed += 1;
            timings.macro_climate_us = t_macro.elapsed().as_micros() as u64;
        }

        // 1. Lazy catch-up for any active LOD-0 chunk lagging behind current macro step (budgeted)
        let t0 = Instant::now();
        catch_up_fine_tick(&mut self.world, &self.active_chunk_mask, 16);
        self.refresh_ready_mask();
        timings.catchup_us = t0.elapsed().as_micros() as u64;

        // 2. Conserved cellular fluid pass across active chunks
        let t1 = Instant::now();
        let tick_parity = self.world.clock.tick.0;
        step_fluid_active_chunks(&mut self.world, &self.ready_active_mask, tick_parity);
        timings.fluid_us = t1.elapsed().as_micros() as u64;

        // 3. L1 local physical electric field solver on configured cadence (e.g. 15 Hz or 20 Hz)
        if fire_electric {
            let t2 = Instant::now();
            solve_local_electric_field(&mut self.world, &self.ready_active_mask);
            timings.electric_us = t2.elapsed().as_micros() as u64;
        }

        // 4. L1 physicochemical & 4-microbe wall-aware fields on meso cadence
        if fire_meso {
            let t3 = Instant::now();
            step_scheduled_fields(&mut self.world, &self.ready_active_mask);
            timings.fields_us = t3.elapsed().as_micros() as u64;
        }

        timings.total_us = t_total.elapsed().as_micros() as u64;
        self.profiler.record(timings);
        era_changed
    }

    /// Advance purely in macro steps (E.4: fast-forward 30 / 365 / 1220+ game days without
    /// wasting CPU on millions of fine ticks).
    /// Returns the number of era transitions occurred during this leap.
    pub fn step_macro_steps(&mut self, count: u64, catch_up_visible: bool) -> u32 {
        let t_total = Instant::now();
        let mut timings = SubsystemTimingsUs::default();
        let mut transitions = 0u32;

        let t_mac = Instant::now();
        for _ in 0..count {
            self.world.clock.advance_macro_steps(1);
            if step_macro_climate(&mut self.world) {
                transitions += 1;
            }
            self.profiler.macro_steps_executed += 1;
        }
        timings.macro_climate_us = t_mac.elapsed().as_micros() as u64;

        if catch_up_visible && self.active_lod0_count > 0 {
            let t_cu = Instant::now();
            catch_up_active_chunks(
                &mut self.world,
                &self.active_chunk_mask,
                self.active_lod0_count as usize,
            );
            timings.catchup_us = t_cu.elapsed().as_micros() as u64;

            self.refresh_ready_mask();
            let t_ef = Instant::now();
            solve_local_electric_field(&mut self.world, &self.ready_active_mask);
            timings.electric_us = t_ef.elapsed().as_micros() as u64;
        }

        timings.total_us = t_total.elapsed().as_micros() as u64;
        self.profiler.record(timings);
        transitions
    }

    /// Advance macro steps stopping early if any basin changes `EraPhase` (breakpoint support for F9).
    pub fn step_macro_until_era_change(&mut self, max_steps: u64) -> (u64, bool) {
        let mut ran = 0u64;
        for _ in 0..max_steps {
            ran += 1;
            let changed = self.step_macro_steps(1, false) > 0;
            if changed {
                catch_up_active_chunks(
                    &mut self.world,
                    &self.active_chunk_mask,
                    self.active_lod0_count as usize,
                );
                return (ran, true);
            }
        }
        catch_up_active_chunks(
            &mut self.world,
            &self.active_chunk_mask,
            self.active_lod0_count as usize,
        );
        (ran, false)
    }

    /// Catch up every chunk in the entire world (used for persistence/consistency checks).
    pub fn catch_up_all_chunks(&mut self) {
        catch_up_world(&mut self.world);
    }

    pub fn build_profiler_snapshot(&self) -> ProfilerSnapshot {
        let (p50, p95, p99) = self.profiler.percentiles();
        ProfilerSnapshot {
            latest: self.profiler.latest,
            p50_total_us: p50,
            p95_total_us: p95,
            p99_total_us: p99,
            estimated_memory_bytes: self.world.estimated_memory_bytes(),
            active_lod0_chunks: self.active_lod0_count,
            total_chunks: self.world.l0_chunks.len() as u32,
            fine_ticks_executed: self.profiler.fine_ticks_executed,
            macro_steps_executed: self.profiler.macro_steps_executed,
        }
    }

    #[inline]
    pub fn active_mask(&self) -> &[bool] {
        &self.active_chunk_mask
    }

    pub fn world_center_of_chunk(cc: ChunkCoord) -> (i32, i32) {
        (
            cc.cx * (CHUNK_SIZE as i32) + (CHUNK_SIZE as i32) / 2,
            cc.cy * (CHUNK_SIZE as i32) + (CHUNK_SIZE as i32) / 2,
        )
    }
}
