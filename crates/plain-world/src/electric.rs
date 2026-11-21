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

    let sky_by_column: Vec<_> = (0..chunks_x * axis)
        .map(|fx| {
            let id = world.region_for_x(fx * ds + ds / 2).0 as usize;
            world.regions.get(id).map_or(10.0, |r| {
                r.charge_index * 120.0
                    + match r.weather {
                        WeatherSpectacle::SilentReverseRain => 180.0,
                        WeatherSpectacle::WanderingBallLightning => 95.0,
                        WeatherSpectacle::AuroraMagStorm => 70.0,
                        WeatherSpectacle::RustRedRain => 55.0,
                        WeatherSpectacle::Calm => 10.0,
                    }
            })
        })
        .collect();
    let balls = lightning_balls(world);
    let surface = &world.topology.surface_height;
    let balls_by_chunk_column: Vec<Vec<_>> = (0..chunks_x)
        .map(|cx| {
            let lo = (cx * CHUNK_SIZE) as f32;
            let hi = lo + CHUNK_SIZE as f32;
            balls
                .iter()
                .filter(|ball| ball.x + ball.radius >= lo && ball.x - ball.radius < hi)
                .collect()
        })
        .collect();

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
                let balls = &balls_by_chunk_column[cx];
                let is_top_chunk = cy + 1 == chunks_y;
                let is_bottom_chunk = cy == 0;

                stencil.clear();
                stencil.resize(axis * axis, ElectricStencil::default());
                // Each open edge has one static conductance for this solve.
                for fy in 0..axis {
                    let wy = cy * CHUNK_SIZE + fy * ds + ds / 2;
                    for fx in 0..axis {
                        let fi = fy * axis + fx;
                        let lx = fx * ds + ds / 2;
                        let ly = fy * ds + ds / 2;
                        let wx = cx * CHUNK_SIZE + lx;
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
                            source = sky_by_column[cx * axis + fx];
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
                        } else if mat.is_air()
                            && !balls.is_empty()
                            && surface.get(wx).is_some_and(|&height| {
                                (usize::from(height) + 10..=usize::from(height) + 140).contains(&wy)
                            })
                        {
                            source = balls
                                .iter()
                                .map(|ball| ball.potential(wx, wy))
                                .fold(0.0, f32::max);
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct LightningBall {
    x: f32,
    y: f32,
    radius: f32,
    start: usize,
    end: usize,
}

impl LightningBall {
    fn potential(self, x: usize, y: usize) -> f32 {
        if !(self.start..self.end).contains(&x) {
            return 0.0;
        }
        let dx = x as f32 - self.x;
        let dy = y as f32 - self.y;
        let falloff = (1.0 - (dx * dx + dy * dy) / (self.radius * self.radius)).max(0.0);
        360.0 * falloff * falloff
    }
}

fn lightning_balls(world: &WorldState) -> Vec<LightningBall> {
    // Adjacent hashed waypoints share an endpoint. Smooth interpolation keeps
    // the motion continuous without replaying a sinusoidal orbit.
    const LEG_TICKS: u64 = 420;
    let leg = world.clock.tick.0 / LEG_TICKS;
    let t = (world.clock.tick.0 % LEG_TICKS) as f32 / LEG_TICKS as f32;
    let blend = t * t * (3.0 - 2.0 * t);
    let mut balls = Vec::new();
    for region in &world.regions {
        if region.weather != WeatherSpectacle::WanderingBallLightning
            || region.x_start >= region.x_end
        {
            continue;
        }
        let key = u64::from(region.id.0) << 8;
        let count = 2 + world.seed.derive_u64("ball-lightning-count", key) % 3;
        let span = (region.x_end - region.x_start) as f32;
        for id in 0..count {
            let waypoint = |leg: u64| {
                let hash = world.seed.derive_u64(
                    "ball-lightning-waypoint",
                    leg.wrapping_mul(65536).wrapping_add(key + id),
                );
                let unit = |shift| ((hash >> shift) as u16) as f32 / f32::from(u16::MAX);
                [unit(0), unit(16), unit(32)]
            };
            let a = waypoint(leg);
            let b = waypoint(leg + 1);
            let [wander, height, size] = std::array::from_fn(|i| a[i] + (b[i] - a[i]) * blend);
            let lane_width = span / count as f32;
            let x = region.x_start as f32 + (id as f32 + 0.15 + 0.7 * wander) * lane_width;
            let column = (x as usize).min(region.x_end - 1);
            let Some(&ground) = world.topology.surface_height.get(column) else {
                continue;
            };
            let next_ground = world
                .topology
                .surface_height
                .get((column + 1).min(region.x_end - 1))
                .copied()
                .unwrap_or(ground);
            let ground =
                f32::from(ground) + (f32::from(next_ground) - f32::from(ground)) * x.fract();
            let bottom = ground + 10.0;
            let top = (ground + 140.0).min(world.height_cells().saturating_sub(2) as f32);
            if bottom > top {
                continue;
            }
            let radius = (9.0 + 7.0 * size).min(lane_width * 0.44).max(2.0);
            let y = bottom + (top - bottom) * (0.2 + 0.6 * height);
            balls.push(LightningBall {
                x,
                y,
                radius,
                start: region.x_start,
                end: region.x_end,
            });
        }
    }
    balls
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
