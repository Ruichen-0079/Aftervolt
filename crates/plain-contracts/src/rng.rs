use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use serde::{Deserialize, Serialize};

/// 256-bit master world seed (F6: hierarchical BLAKE3 domain-separated ChaCha20 streams).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MasterSeed(pub [u8; 32]);

impl MasterSeed {
    pub fn from_u64(value: u64) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"electroruin-plain-master-seed-v1:");
        hasher.update(&value.to_le_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    pub fn from_str_label(label: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"electroruin-plain-master-label-v1:");
        hasher.update(label.as_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    /// Derive a domain-separated deterministic ChaCha20 RNG stream.
    /// Guarantees changes to one domain (e.g., ecology) never perturb another (e.g., terrain).
    pub fn derive_rng(&self, domain: &str, stream_index: u64) -> ChaCha20Rng {
        let mut hasher = blake3::Hasher::new_keyed(&self.0);
        hasher.update(domain.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(&stream_index.to_le_bytes());
        ChaCha20Rng::from_seed(*hasher.finalize().as_bytes())
    }

    /// Deterministic 64-bit hash key for coordinate-based procedural noise.
    pub fn derive_u64(&self, domain: &str, stream_index: u64) -> u64 {
        let mut hasher = blake3::Hasher::new_keyed(&self.0);
        hasher.update(domain.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(&stream_index.to_le_bytes());
        let bytes = hasher.finalize();
        u64::from_le_bytes([
            bytes.as_bytes()[0],
            bytes.as_bytes()[1],
            bytes.as_bytes()[2],
            bytes.as_bytes()[3],
            bytes.as_bytes()[4],
            bytes.as_bytes()[5],
            bytes.as_bytes()[6],
            bytes.as_bytes()[7],
        ])
    }

    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            use std::fmt::Write;
            let _ = write!(&mut s, "{:02x}", b);
        }
        s
    }
}

// =========================================================================
// Deterministic cross-platform math wrappers via pure-Rust `libm` (F6).
// Prevents x86/ARM/OS glibc-vs-MSVC transcendental drift.
// =========================================================================

#[inline]
pub fn det_sin(x: f64) -> f64 {
    libm::sin(x)
}

#[inline]
pub fn det_cos(x: f64) -> f64 {
    libm::cos(x)
}

#[inline]
pub fn det_sqrt(x: f64) -> f64 {
    libm::sqrt(x)
}

#[inline]
pub fn det_pow(base: f64, exp: f64) -> f64 {
    libm::pow(base, exp)
}

#[inline]
pub fn det_exp(x: f64) -> f64 {
    libm::exp(x)
}

#[inline]
pub fn det_floor(x: f32) -> f32 {
    libm::floorf(x)
}

#[inline]
pub fn det_floor_f64(x: f64) -> f64 {
    libm::floor(x)
}
