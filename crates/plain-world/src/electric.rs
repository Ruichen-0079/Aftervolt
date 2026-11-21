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

#[derive(Clone, Copy, Default)]
struct ElectricStencil {
    weights: [f32; 4],
    numerator: f32,
    denominator: f32,
    fixed: bool,
}

#[inline]
fn edge_conductance(a: u16, b: u16) -> f32 {
    0.5 * (f32::from(a.max(5)) + f32::from(b.max(5)))
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

    // The local lightning forcing is separable across each axis. Evaluate
    // libm once per row/column instead of repeating the same values per tile.
    let lightning_x: Vec<Option<Vec<f64>>> = col_forcing
        .iter()
        .enumerate()
        .map(|(cx, &(_, weather))| {
            (weather == WeatherSpectacle::WanderingBallLightning).then(|| {
                (0..axis)
                    .map(|fx| det_sin((cx * CHUNK_SIZE + fx * ds) as f64 * 0.015 + tick_phase))
                    .collect()
            })
        })
        .collect();
    let lightning_y: Vec<f64> = if lightning_x.iter().any(Option::is_some) {
        (0..chunks_y * axis)
            .map(|fy| det_cos((fy * ds) as f64 * 0.02 - tick_phase * 0.7))
            .collect()
    } else {
        Vec::new()
    };

    let halos = crate::halo::snapshot(world, active_mask, |l1, i| {
        (l1.electric_potential[i], l1.conductivity[i])
    });

    // Parallel per-chunk Red-Black Gauss-Seidel relaxation (zero data races, deterministic).
    world
        .l1_chunks
        .par_iter_mut()
        .zip(world.l0_chunks.par_iter_mut())
        .enumerate()
        .for_each_init(
            || (Vec::<ElectricStencil>::new(), [Vec::new(), Vec::new()]),
            |(stencil, colors), (ci, (l1, l0))| {
                if !active_mask[ci] {
                    return;
                }
                l0.sleep.dirty_for_save = true;
                let cx = ci % chunks_x;
                let cy = ci / chunks_x;
                let (sky_charge_v, _) = col_forcing[cx];
                let wave_x = lightning_x[cx].as_deref();
                let is_top_chunk = cy + 1 == chunks_y;
                let is_bottom_chunk = cy == 0;

                stencil.clear();
                stencil.resize(axis * axis, ElectricStencil::default());
                // Each open edge has one static conductance for this solve.
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
                            let weight =
                                edge_conductance(l1.conductivity[fi], l1.conductivity[fi + axis]);
                            stencil[fi].weights[0] = weight;
                            stencil[fi + axis].weights[1] = weight;
                        }
                        if fx + 1 < axis
                            && mask & FLUX_BLOCK_E == 0
                            && l1.flux_block_mask[fi + 1] & FLUX_BLOCK_W == 0
                        {
                            let weight =
                                edge_conductance(l1.conductivity[fi], l1.conductivity[fi + 1]);
                            stencil[fi].weights[2] = weight;
                            stencil[fi + 1].weights[3] = weight;
                        }

                        let c_self = l1.conductivity[fi].max(5) as f32;
                        let mut source = 0.0;
                        if is_top_chunk && fy + 1 == axis {
                            // Sky boundary electrostatic potential
                            source = sky_charge_v;
                            stencil[fi].fixed = true;
                        } else if is_bottom_chunk && fy == 0 {
                            // Deep bedrock ground reference (0 V)
                            source = 0.0;
                            stencil[fi].fixed = true;
                        } else if mat == MaterialId::RUIN_HEAT_CORE {
                            // Deep ruin residual low-power emitter
                            source = 85.0;
                            stencil[fi].fixed = true;
                        } else if mat == MaterialId::CONDUCTIVE_ORE
                            || mat == MaterialId::RUIN_CONDUIT_CASING
                        {
                            source = 14.0;
                        } else if mat.is_air() && wy > (chunks_y * CHUNK_SIZE) / 3 {
                            if let Some(wave_x) = wave_x {
                                let wave = wave_x[fx] * lightning_y[cy * axis + fy];
                                if wave > 0.72 {
                                    source = ((wave - 0.72) as f32) * 320.0;
                                }
                            }
                        }
                        stencil[fi].numerator = if stencil[fi].fixed {
                            source
                        } else {
                            source * 0.15 * c_self
                        };
                        stencil[fi].denominator = 0.15 * c_self + 1.0;
                    }
                }

                let edge = axis - 1;
                for (slot, remote) in halos[ci].iter().enumerate() {
                    if let Some((potential, conductivity)) = *remote {
                        let k = slot % axis;
                        let fi = match slot / axis {
                            0 => edge * axis + k,
                            1 => k,
                            2 => k * axis + edge,
                            _ => k * axis,
                        };
                        if !stencil[fi].fixed {
                            let weight = edge_conductance(l1.conductivity[fi], conductivity);
                            stencil[fi].numerator += weight * potential;
                            stencil[fi].denominator += weight;
                        }
                    }
                }

                for color in colors.iter_mut() {
                    color.clear();
                }
                for (fi, tile) in stencil.iter_mut().enumerate() {
                    let conductance = tile.weights.iter().sum::<f32>();
                    tile.denominator = (tile.denominator + conductance).recip();
                    if conductance > 0.0 || tile.fixed {
                        colors[(fi / axis + fi % axis) % 2].push(fi);
                    } else {
                        // An isolated tile does not affect any other local tile.
                        let target = tile.numerator * tile.denominator;
                        let mut voltage = l1.electric_potential[fi];
                        if voltage != target {
                            for _ in 0..iters {
                                voltage = relax_voltage(voltage, target);
                            }
                        }
                        l1.electric_potential[fi] = voltage.clamp(-500.0, 1000.0);
                    }
                }

                // Red-Black Gauss-Seidel relaxation on the coupled tiles.
                for _ in 0..iters {
                    for color in colors.iter() {
                        for &fi in color {
                            let tile = stencil[fi];
                            if tile.fixed {
                                l1.electric_potential[fi] = tile.numerator;
                                continue;
                            }
                            let [north, south, east, west] = tile.weights;
                            let mut num = tile.numerator;
                            if north > 0.0 {
                                num += north * l1.electric_potential[fi + axis];
                            }
                            if south > 0.0 {
                                num += south * l1.electric_potential[fi - axis];
                            }
                            if east > 0.0 {
                                num += east * l1.electric_potential[fi + 1];
                            }
                            if west > 0.0 {
                                num += west * l1.electric_potential[fi - 1];
                            }
                            let updated = num * tile.denominator;
                            l1.electric_potential[fi] =
                                relax_voltage(l1.electric_potential[fi], updated);
                        }
                    }
                }
            },
        );
}

#[inline]
fn relax_voltage(previous: f32, target: f32) -> f32 {
    (previous + 1.15 * (target - previous)).clamp(-500.0, 1000.0)
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
