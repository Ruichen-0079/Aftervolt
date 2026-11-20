use plain_contracts::rng::det_floor_f64;

/// Pure integer-hashed deterministic gradient/value noise using `libm` (100% cross-platform identical).
#[inline]
fn hash_u64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[inline]
fn hash_2d(seed: u64, ix: i64, iy: i64) -> f64 {
    let h = hash_u64(
        seed ^ (ix as u64).wrapping_mul(0x517C_C1B7_2722_0A95)
            ^ (iy as u64).wrapping_mul(0x2545_F491_4F6C_DD1D),
    );
    // Map top 53 bits uniformly to [-1.0, 1.0]
    let u = ((h >> 11) as f64) * (1.0 / ((1u64 << 53) as f64));
    u * 2.0 - 1.0
}

#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Deterministic smooth 1D value noise in [-1.0, 1.0].
pub fn noise1d(seed: u64, x: f64) -> f64 {
    let fx = det_floor_f64(x);
    let ix = fx as i64;
    let tx = fade(x - fx);
    let v0 = hash_2d(seed, ix, 0);
    let v1 = hash_2d(seed, ix + 1, 0);
    lerp(v0, v1, tx)
}

/// Deterministic smooth 2D value noise in [-1.0, 1.0].
pub fn noise2d(seed: u64, x: f64, y: f64) -> f64 {
    let fx = det_floor_f64(x);
    let fy = det_floor_f64(y);
    let ix = fx as i64;
    let iy = fy as i64;
    let tx = fade(x - fx);
    let ty = fade(y - fy);

    let c00 = hash_2d(seed, ix, iy);
    let c10 = hash_2d(seed, ix + 1, iy);
    let c01 = hash_2d(seed, ix, iy + 1);
    let c11 = hash_2d(seed, ix + 1, iy + 1);

    lerp(lerp(c00, c10, tx), lerp(c01, c11, tx), ty)
}

/// Deterministic 1D fractal Brownian motion in [-1.0, 1.0].
pub fn fbm1d(seed: u64, x: f64, octaves: u32) -> f64 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for o in 0..octaves {
        sum += noise1d(seed.wrapping_add((o as u64) * 1013), x * freq) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.03;
    }
    if norm > 0.0 { sum / norm } else { 0.0 }
}

/// Deterministic 1D ridged multifractal noise in [0.0, 1.0] (ideal for jagged ridge plateaus).
pub fn ridged1d(seed: u64, x: f64, octaves: u32) -> f64 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for o in 0..octaves {
        let n = 1.0 - noise1d(seed.wrapping_add((o as u64) * 2027), x * freq).abs();
        sum += (n * n) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.1;
    }
    if norm > 0.0 { sum / norm } else { 0.0 }
}

/// Deterministic 2D domain-warped fBm in [-1.0, 1.0].
pub fn fbm2d(seed: u64, x: f64, y: f64, octaves: u32) -> f64 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for o in 0..octaves {
        let s = seed.wrapping_add((o as u64) * 4099);
        sum += noise2d(s, x * freq, y * freq) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.02;
    }
    if norm > 0.0 { sum / norm } else { 0.0 }
}

pub fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
