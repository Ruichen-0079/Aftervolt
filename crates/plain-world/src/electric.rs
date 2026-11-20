use plain_contracts::materials::MaterialId;
use plain_contracts::rng::{det_cos, det_sin};
use plain_contracts::telemetry::WeatherSpectacle;
use plain_contracts::{CellCoord, ChunkCoord, WorldPosMeters, CHUNK_SIZE};
use rayon::prelude::*;

use crate::spatial::{WorldState, FLUX_BLOCK_E, FLUX_BLOCK_N, FLUX_BLOCK_S, FLUX_BLOCK_W};

/// Result of O(1) physical electroreception sampling at a world position (F7 non-cheating contract).
#[derive(Debug, Clone, Copy, Default)]
pub struct ElectricSample {
    pub potential_v: f32,
    pub grad_x: f32,
    pub grad_y: f32,
    pub noise_amplitude: f32,
    pub local_conductivity: u16,
}

/// Solve local 2D physical electric potential across active chunks' L1 field tiles.
/// Uses deterministic wall-aware conductance-weighted Jacobi/Red-Black relaxation with
/// configurable iteration count (`config.electric_solver_iterations`, Q2/Q3).
pub fn solve_local_electric_field(world: &mut WorldState, active_mask: &[bool]) {
    let iters = world.config.electric_solver_iterations.max(1) as usize;
    let axis = world.config.field_downsample.tiles_per_chunk_axis();
    let ds = world.config.field_downsample.factor();
    let chunks_x = world.config.dimensions.chunks_x as usize;
    let chunks_y = world.config.dimensions.chunks_y as usize;

    for (ci, &active) in active_mask.iter().enumerate() {
        if active && world.l0_chunks[ci].sleep.flux_mask_dirty {
            world.recompute_chunk_flux_mask(ci);
        }
    }

    // Precompute regional atmospheric forcing per chunk column
    let col_forcing: Vec<(f32, WeatherSpectacle)> = (0..chunks_x)
        .map(|cx| {
            let x_mid = cx * CHUNK_SIZE + CHUNK_SIZE / 2;
            let rid = world.region_for_x(x_mid).0 as usize;
            let r = &world.regions[rid.min(world.regions.len().saturating_sub(1))];
            let charge_v = (r.charge_index * 120.0)
                + match r.weather {
                    WeatherSpectacle::SilentReverseRain => 180.0,
                    WeatherSpectacle::WanderingBallLightning => 95.0,
                    WeatherSpectacle::AuroraMagStorm => 70.0,
                    WeatherSpectacle::RustRedRain => 55.0,
                    WeatherSpectacle::Calm => 10.0,
                };
            (charge_v, r.weather)
        })
        .collect();

    let tick_phase = (world.clock.tick.0 as f64) * 0.05;

    // Parallel per-chunk Red-Black Gauss-Seidel relaxation (zero data races, deterministic).
    world
        .l1_chunks
        .par_iter_mut()
        .zip(world.l0_chunks.par_iter_mut())
        .enumerate()
        .for_each(|(ci, (l1, l0))| {
            if !active_mask[ci] {
                return;
            }
            l0.sleep.dirty_for_save = true;
            let cx = ci % chunks_x;
            let cy = ci / chunks_x;
            let (sky_charge_v, weather) = col_forcing[cx];
            let is_top_chunk = cy + 1 == chunks_y;
            let is_bottom_chunk = cy == 0;

            // Identify ruin/ore emitters and surface boundary forcing once before iterations
            let n = axis * axis;
            let mut source_v = vec![0.0f32; n];
            let mut is_dirichlet = vec![false; n];
            // Resolve both faces once per edge before the relaxation sweeps.
            let mut open_faces = vec![0u8; n];

            for fy in 0..axis {
                let wy = cy * CHUNK_SIZE + fy * ds;
                for fx in 0..axis {
                    let fi = fy * axis + fx;
                    let lx = fx * ds + ds / 2;
                    let ly = fy * ds + ds / 2;
                    let mat = MaterialId(l0.solid_material[ly * CHUNK_SIZE + lx]);
                    let mask = l1.flux_block_mask[fi];
                    if fy + 1 < axis
                        && mask & FLUX_BLOCK_N == 0
                        && l1.flux_block_mask[fi + axis] & FLUX_BLOCK_S == 0
                    {
                        open_faces[fi] |= FLUX_BLOCK_N;
                        open_faces[fi + axis] |= FLUX_BLOCK_S;
                    }
                    if fx + 1 < axis
                        && mask & FLUX_BLOCK_E == 0
                        && l1.flux_block_mask[fi + 1] & FLUX_BLOCK_W == 0
                    {
                        open_faces[fi] |= FLUX_BLOCK_E;
                        open_faces[fi + 1] |= FLUX_BLOCK_W;
                    }

                    if is_top_chunk && fy + 1 == axis {
                        // Sky boundary electrostatic potential
                        source_v[fi] = sky_charge_v;
                        is_dirichlet[fi] = true;
                    } else if is_bottom_chunk && fy == 0 {
                        // Deep bedrock ground reference (0 V)
                        source_v[fi] = 0.0;
                        is_dirichlet[fi] = true;
                    } else if mat == MaterialId::RUIN_HEAT_CORE {
                        // Deep ruin residual low-power emitter
                        source_v[fi] = 85.0;
                        is_dirichlet[fi] = true;
                    } else if mat == MaterialId::CONDUCTIVE_ORE
                        || mat == MaterialId::RUIN_CONDUIT_CASING
                    {
                        source_v[fi] = 14.0;
                    } else if weather == WeatherSpectacle::WanderingBallLightning
                        && mat.is_air()
                        && wy > (chunks_y * CHUNK_SIZE) / 3
                    {
                        // Moving localized ball-lightning potential pockets
                        let wx = (cx * CHUNK_SIZE + fx * ds) as f64;
                        let wave = det_sin(wx * 0.015 + tick_phase)
                            * det_cos((wy as f64) * 0.02 - tick_phase * 0.7);
                        if wave > 0.72 {
                            source_v[fi] = ((wave - 0.72) as f32) * 320.0;
                        }
                    }
                }
            }

            // Red-Black Gauss-Seidel relaxation obeying L0-derived wall-aware flux_block_mask
            for _ in 0..iters {
                for parity in 0..2 {
                    for fy in 0..axis {
                        for fx in ((fy + parity) % 2..axis).step_by(2) {
                            let fi = fy * axis + fx;
                            if is_dirichlet[fi] {
                                l1.electric_potential[fi] = source_v[fi];
                                continue;
                            }
                            let open = open_faces[fi];
                            let c_self = (l1.conductivity[fi].max(5)) as f32;
                            let mut num = source_v[fi] * 0.15 * c_self;
                            let mut den = 0.15 * c_self + 1.0;

                            if open & FLUX_BLOCK_N != 0 {
                                let neighbor = fi + axis;
                                let w = 0.5 * (c_self + l1.conductivity[neighbor].max(5) as f32);
                                num += w * l1.electric_potential[neighbor];
                                den += w;
                            }
                            if open & FLUX_BLOCK_S != 0 {
                                let neighbor = fi - axis;
                                let w = 0.5 * (c_self + l1.conductivity[neighbor].max(5) as f32);
                                num += w * l1.electric_potential[neighbor];
                                den += w;
                            }
                            if open & FLUX_BLOCK_E != 0 {
                                let neighbor = fi + 1;
                                let w = 0.5 * (c_self + l1.conductivity[neighbor].max(5) as f32);
                                num += w * l1.electric_potential[neighbor];
                                den += w;
                            }
                            if open & FLUX_BLOCK_W != 0 {
                                let neighbor = fi - 1;
                                let w = 0.5 * (c_self + l1.conductivity[neighbor].max(5) as f32);
                                num += w * l1.electric_potential[neighbor];
                                den += w;
                            }

                            let updated = num / den;
                            // Mild SOR blend (omega = 1.15) clamped for unconditional stability
                            let prev = l1.electric_potential[fi];
                            l1.electric_potential[fi] =
                                (prev + 1.15 * (updated - prev)).clamp(-500.0, 1000.0);
                        }
                    }
                }
            }
        });
}

/// O(1) physical electroreception probe at an arbitrary world position (meters).
/// Computes central-difference electric gradient + environmental/aurora noise without reading any entity coordinates.
pub fn sample_electric_gradient(
    world: &WorldState,
    pos: WorldPosMeters,
    include_weather_noise: bool,
) -> ElectricSample {
    let ds = world.config.field_downsample.factor() as i32;
    let center_cell = pos.to_cell(world.config.meters_per_cell);
    if !world.config.dimensions.contains_cell(center_cell) {
        return ElectricSample::default();
    }

    let read_tile = |cell: CellCoord| {
        let ci = world.chunk_index(cell.chunk_coord())?;
        let fx = cell.x.rem_euclid(CHUNK_SIZE as i32) as usize / ds as usize;
        let fy = cell.y.rem_euclid(CHUNK_SIZE as i32) as usize / ds as usize;
        let l1 = &world.l1_chunks[ci];
        let fi = l1.idx(fx, fy);
        Some((
            l1.electric_potential[fi],
            l1.conductivity[fi],
            l1.flux_block_mask[fi],
        ))
    };

    let Some((vc, cond, center_mask)) = read_tile(center_cell) else {
        return ElectricSample::default();
    };
    let neighbor_v = |dx, dy, outgoing, incoming| {
        if center_mask & outgoing != 0 {
            return vc;
        }
        read_tile(CellCoord::new(center_cell.x + dx, center_cell.y + dy))
            .filter(|&(_, _, mask)| mask & incoming == 0)
            .map_or(vc, |(potential, _, _)| potential)
    };
    let vr = neighbor_v(ds, 0, FLUX_BLOCK_E, FLUX_BLOCK_W);
    let vl = neighbor_v(-ds, 0, FLUX_BLOCK_W, FLUX_BLOCK_E);
    let vu = neighbor_v(0, ds, FLUX_BLOCK_N, FLUX_BLOCK_S);
    let vd = neighbor_v(0, -ds, FLUX_BLOCK_S, FLUX_BLOCK_N);

    let span_m = (2.0 * (ds as f32) * world.config.meters_per_cell).max(0.1);
    let mut grad_x = (vr - vl) / span_m;
    let mut grad_y = (vu - vd) / span_m;
    let mut noise_amp = 0.0f32;

    if include_weather_noise {
        let rid = world.region_for_x(center_cell.x as usize).0 as usize;
        let weather = world
            .regions
            .get(rid)
            .map(|r| r.weather)
            .unwrap_or(WeatherSpectacle::Calm);
        let t = (world.clock.tick.0 as f64) * 0.13;
        match weather {
            WeatherSpectacle::AuroraMagStorm => {
                // Injects deterministic phantom geomagnetic signals (Aurora illusion night)
                noise_amp = 18.0;
                grad_x += (det_sin(pos.x * 0.07 + t) * 18.0) as f32;
                grad_y += (det_cos(pos.y * 0.07 - t * 1.1) * 14.0) as f32;
            }
            WeatherSpectacle::SilentReverseRain => {
                noise_amp = 2.5;
                grad_y += 12.0; // Strong upward static pull
            }
            WeatherSpectacle::RustRedRain | WeatherSpectacle::WanderingBallLightning => {
                noise_amp = 7.0;
                grad_x += (det_sin(pos.x * 0.2 + t * 2.3) * 5.0) as f32;
            }
            WeatherSpectacle::Calm => {
                noise_amp = 0.5;
            }
        }
    }

    ElectricSample {
        potential_v: vc,
        grad_x,
        grad_y,
        noise_amplitude: noise_amp,
        local_conductivity: cond,
    }
}

/// Helper for getting chunk index range covered by a viewport rectangle.
pub fn viewport_chunk_bounds(
    world: &WorldState,
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
    pad: u16,
) -> (ChunkCoord, ChunkCoord) {
    let p = pad as i32;
    let min_c = CellCoord::new(min_x, min_y).chunk_coord();
    let max_c = CellCoord::new(max_x, max_y).chunk_coord();
    (
        ChunkCoord::new((min_c.cx - p).max(0), (min_c.cy - p).max(0)),
        ChunkCoord::new(
            (max_c.cx + p).min(world.config.dimensions.chunks_x as i32 - 1),
            (max_c.cy + p).min(world.config.dimensions.chunks_y as i32 - 1),
        ),
    )
}
