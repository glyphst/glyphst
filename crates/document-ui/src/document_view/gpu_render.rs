use super::*;
use std::collections::VecDeque;

mod diagnostics;
mod preparation;
mod presentation;
mod state;
#[cfg(test)]
mod tests;

use diagnostics::*;
use preparation::*;
use presentation::*;
pub(super) use state::GpuPdfState;
use state::*;

const MAX_RETAINED_GPU_PAGES: usize = 8;
const MAX_RETAINED_GPU_DIAGNOSTICS: usize = 64;
const MAX_CONCURRENT_GPU_PAGE_PREPARATIONS: usize = 2;
const MAX_RETAINED_GPU_RESOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RETAINED_GPU_SURFACE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SURFACE_LATENCY_SAMPLES: usize = 120;
const GPU_TARGET_ALIGNMENT: u32 = 128;
const GPU_PAN_CACHE_GUTTER_MIN: u32 = 96;
const GPU_PAN_AXIS_JITTER_PX: u32 = 4;
const GPU_FULL_PAGE_CACHE_MIN_MOTION_SEGMENTS: u64 = 300_000;
const GPU_PAN_CACHE_TARGET_COUNT: u64 = 3;
const GPU_CAMERA_TARGET_COUNT: u64 = 2;
const GPU_MOTION_MIN_DENSITY_NUMERATOR: u64 = 9;
const GPU_MOTION_MIN_DENSITY_DENOMINATOR: u64 = 10;

const GPU_MOTION_SETTLE_DELAY: Duration = Duration::from_millis(100);
const GPU_SPECULATIVE_RESUME_DELAY: Duration = Duration::from_millis(400);
const GPU_FIRST_PAINT_GRACE: Duration = Duration::from_millis(50);
