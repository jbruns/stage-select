//! What a Calibration measured (ADR 0022): for the Host, how fast hash workers checksum as their count grows, and for
//! each Root, how fast payload readers read from it. Each sweep's knee is the Ceiling it suggests.

use serde::{Deserialize, Serialize};

/// One step of a sweep: `workers` at work together, and what they got through.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub workers: usize,
    /// Bytes hashed (the Host) or read (a Root) during the step.
    pub bytes: u64,
    pub duration_ms: u64,
    pub bytes_per_s: f64,
}

// Throughput is a count over a positive span, never NaN.
impl Eq for CurvePoint {}

/// The smallest worker count within 10% of a sweep's best throughput, never one past a drop, and the throughput it
/// reached.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Knee {
    pub workers: usize,
    pub bytes_per_s: f64,
}

impl Eq for Knee {}

/// What the latest Calibration that measured the Host found: hash workers over synthetic data in memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCalibration {
    /// The Calibration Job that measured it.
    pub job: i64,
    pub measured_at: String,
    /// The CPUs the Service could use then.
    pub cpus: usize,
    pub curve: Vec<CurvePoint>,
    pub knee: Option<Knee>,
}

/// How a Root's measurement ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootOutcome {
    /// Its readers were swept, and its curve and knee recorded.
    Measured,
    /// It held too little to read for a measurement that means anything.
    TooSmall,
    /// It couldn't be read at all.
    Unreadable,
}

/// What the latest Calibration that measured a Root found: payload readers over its least recently read files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCalibration {
    pub root: String,
    /// The Root's path when it was measured.
    pub path: String,
    /// The Calibration Job that measured it.
    pub job: i64,
    pub measured_at: String,
    pub outcome: RootOutcome,
    /// The bytes it found to read, up to what a full sweep needs.
    pub available_bytes: u64,
    pub curve: Vec<CurvePoint>,
    pub knee: Option<Knee>,
    /// Why it couldn't be read, when it couldn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Why the Calibration results are Stale. Stale results keep applying until you calibrate again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CalibrationStale {
    /// The Service can use a different number of CPUs from when the Host was measured.
    Cpus { measured: usize, now: usize },
    /// A Root is configured that no Calibration measured.
    Unmeasured { root: String },
    /// A Root's path differs from the one measured.
    Moved {
        root: String,
        measured: String,
        now: String,
    },
}

/// The Calibration results that apply now: the latest measurement of the Host and of each configured Root. A
/// Calibration you cancelled keeps what it finished, so a Root's measurement may come from an earlier one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calibration {
    /// None until a Calibration measures the Host.
    pub host: Option<HostCalibration>,
    /// Each configured Root a Calibration measured, by name.
    pub roots: Vec<RootCalibration>,
    /// Empty while the results are current, and when nothing was ever measured.
    pub stale: Vec<CalibrationStale>,
}

impl Calibration {
    /// Whether any Calibration measured anything.
    pub fn measured(&self) -> bool {
        self.host.is_some() || !self.roots.is_empty()
    }
}
