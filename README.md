# Aftervolt（《遗电平原》）

**Aftervolt** is an independent, deterministic 2D physical and ecological simulation engine (`Rust World Core`) powering *Aftervolt* (*《遗电平原》*).

Built from the ground up in pure Rust without game-engine coupling, Aftervolt simulates configurable finite 2D worlds featuring multi-basin geology, conserved integer cellular hydrology, wall-aware physicochemical & microbial fields, local physical electrostatics, a non-uniform non-linear **1220-game-day four-era macro climate cycle**, lazy cumulative-flux catch-up for dormant chunks, and single-file SQLite WAL + Zstd persistence.

---

## Architecture Principles (Frozen Contracts `F1–F9`)

- **F1 — Engine-Independent Core**: Standalone Rust workspace with zero game-engine glue in the core simulation layers.
- **F2 — Single Physical Authority**: Rust owns the authoritative physical & ecological simulation state; renderers act strictly as read-only projections and command dispatchers.
- **F3 — Configurable Scale & Discrete Clock**: Configurable `WorldDimensions`, physical meter scale (`meters_per_cell`), fixed `u64` simulation ticks, and a configurable **1220-game-day** four-era macro cycle with non-uniform durations and hysteresis transitions.
- **F4 — Four-Tier Spatial Resolution**:
  - `L0`: `64×64` fine terrain & cellular fluid chunks (SoA).
  - `L1`: Downsampled (`4×4` / `2×2`) physicochemical, 4-microbe, and electric potential field tiles, guarded by L0-derived bidirectional `flux_block_mask` to prevent thin-wall leakage.
  - `L2`: Chunk-level scheduling & dirty/sleep tracking.
  - `L3`: Macro basins (`MacroRegion`) owning conserved atmospheric vapor/dust reservoirs and cross-basin wind transport.
- **F5 — Pure Physical Material Law**: Terrain wear, dissolution, and mechanical failure follow `MaterialDef` physical parameters — no hardcoded `indestructible` boolean flags.
- **F6 — Integer Conservation & Bit-Exact Determinism**: Conserved quantities (water in `µL`, charge, etc.) use exact integer accounting; PRNG & hashing use deterministic `ChaCha20` / `BLAKE3` / `libm`.
- **F7 — Compile-Time Truth vs. Perception Isolation**: Strict module and type boundaries between `WorldTruth`, developer telemetry (`DevTelemetryPort`), and future physical perception (`PlayerPerceptionPort`).
- **F8 — Single-File SQLite WAL Persistence**: Full Zstd-compressed `world_baseline` + atomic incremental `dirty_chunks` + explicit dual versioning (`schema_version` / `worldgen_version`) with schema migration support.
- **F9 — Lightweight Real-Time Developer Observer**: Native `eframe`/`egui` observer for live world inspection, environmental overlays, time stepping, and profiler telemetry, decoupled from player perception.

---

## Workspace Structure

```text
crates/
├── plain-contracts   # Core types, coordinates, conserved units, MaterialDef, WorldConfig, deterministic RNG
├── plain-world       # L0/L1/L3 spatial state, cellular fluid, wall-aware fields, electric solver, 1220-day climate, lazy catch-up, scheduler
├── plain-worldgen    # Deterministic finite 2D geological/hydrological world generator & automated legality validator
├── plain-persist     # Single-file SQLite WAL + Zstd baseline/incremental store, schema migration, full-state BLAKE3 hash
├── plain-observer    # Real-time eframe/egui developer observer & CLI verification runner
└── plain-bench       # Quantitative benchmark suite (Q1–Q4 & Q7)
```

---

## Quick Start

Requires Rust (2021 edition, tested on `rustc 1.98`).

### 1. Build & Run Full Test Suite

```bash
RUSTFLAGS="-D warnings" cargo test --workspace --release
```

### 2. Launch the Real-Time Developer Observer (GUI)

```bash
cargo run --release -p plain-observer -- --seed 42 --chunks-x 96 --chunks-y 32
```

### 3. Run Headless Verification Gates

```bash
# 100-seed legality & bit-exact determinism sweep (1024x512)
cargo run --release -p plain-observer -- validate-seeds --count 100 --chunks-x 16 --chunks-y 8

# 3 x 1220 = 3660 game days conservation & four-era oscillation check (6144x2048)
cargo run --release -p plain-observer -- sim-cycles --seed 42 --chunks-x 96 --chunks-y 32 --days 3660

# Bit-exact SQLite WAL baseline + incremental save/load roundtrip (6144x2048)
cargo run --release -p plain-observer -- verify-persistence --seed 42 --chunks-x 96 --chunks-y 32

# Quantitative performance & lazy catch-up benchmarks (Q1-Q4, Q7)
cargo run --release -p plain-bench
```

---

## Current Status

- **Stage 1 (World Core Foundation)**: Implemented and independently verified — deterministic multi-scale worldgen, 100/100 seed legality pass, `0 µL` exact water conservation over `3×1220` game days, bidirectional thin-wall `L1` field/electric isolation, V1→V2 SQLite WAL + Zstd persistence roundtrip, and real-time `eframe`/`egui` developer observer.

---

## Documentation

- Full game design & engineering specification (Chinese): [plain.md](./plain.md)
- Multi-agent engineering governance & architecture baseline: [AGENTS.md](./AGENTS.md)

---

## License

Licensed under the [MIT License](./LICENSE).
