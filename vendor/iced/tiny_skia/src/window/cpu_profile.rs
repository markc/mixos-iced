// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opt-in CPU presentation timing. Disabled frames never read the clock.
//! ICED_CPU_PROFILE=1 enables one aggregate per active second through `log`.
//! Counts contain no document content, paths, window titles or input data.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("ICED_CPU_PROFILE").is_ok_and(|value| value == "1"))
}

pub(super) struct Frame {
    started: Instant,
    phase: Instant,
    us: [u64; 4],
    age: u8,
    width: u32,
    height: u32,
    regions: u64,
    pixels: u64,
    full_region: bool,
}

impl Frame {
    pub(super) fn start() -> Option<Self> {
        if !enabled() {
            return None;
        }
        let now = Instant::now();
        Some(Self {
            started: now,
            phase: now,
            us: [0; 4],
            age: 0,
            width: 0,
            height: 0,
            regions: 0,
            pixels: 0,
            full_region: false,
        })
    }

    fn finish_phase(&mut self, index: usize) {
        let now = Instant::now();
        self.us[index] = now.duration_since(self.phase).as_micros() as u64;
        self.phase = now;
    }

    pub(super) fn acquired(&mut self, age: u8, width: u32, height: u32) {
        self.finish_phase(0);
        self.age = age;
        self.width = width;
        self.height = height;
    }

    pub(super) fn raster_started(&mut self, damage: &[softbuffer::Rect]) {
        self.regions = damage.len() as u64;
        // Sum of submitted rectangles; overlapping pixels are counted more
        // than once, exactly as repeated rasterisation work is.
        self.pixels = damage.iter().fold(0u64, |sum, rect| {
            sum.saturating_add(u64::from(rect.width.get()) * u64::from(rect.height.get()))
        });
        self.full_region = damage.iter().any(|rect| {
            rect.x == 0
                && rect.y == 0
                && rect.width.get() == self.width
                && rect.height.get() == self.height
        });
        self.finish_phase(1);
    }

    pub(super) fn raster_finished(&mut self) {
        self.finish_phase(2);
    }

    pub(super) fn presented(mut self, success: bool) {
        self.finish_phase(3);
        let total = self.phase.duration_since(self.started).as_micros() as u64;
        SAMPLES.with(|samples| samples.borrow_mut().record(self, total, success));
    }
}

#[derive(Default)]
struct Samples {
    since: Option<Instant>,
    frames: u64,
    failed: u64,
    age_zero: u64,
    age_min: Option<u8>,
    age_max: u8,
    full: u64,
    empty: u64,
    width: u32,
    height: u32,
    regions: u64,
    pixels: u64,
    pixels_max: u64,
    us: [u64; 4],
    total_us: u64,
    total_max_us: u64,
}

thread_local! {
    static SAMPLES: RefCell<Samples> = RefCell::new(Samples::default());
}

impl Samples {
    fn record(&mut self, frame: Frame, total: u64, success: bool) {
        let since = *self.since.get_or_insert(frame.phase);
        self.frames += 1;
        self.failed += u64::from(!success);
        self.age_zero += u64::from(frame.age == 0);
        self.age_min = Some(self.age_min.map_or(frame.age, |age| age.min(frame.age)));
        self.age_max = self.age_max.max(frame.age);
        self.full += u64::from(frame.full_region);
        self.empty += u64::from(frame.regions == 0);
        self.width = frame.width;
        self.height = frame.height;
        self.regions += frame.regions;
        self.pixels = self.pixels.saturating_add(frame.pixels);
        self.pixels_max = self.pixels_max.max(frame.pixels);
        for (sum, us) in self.us.iter_mut().zip(frame.us) {
            *sum = sum.saturating_add(us);
        }
        self.total_us = self.total_us.saturating_add(total);
        self.total_max_us = self.total_max_us.max(total);
        if frame.phase.duration_since(since) < Duration::from_secs(1) {
            return;
        }
        let [acquire, damage, raster, present] = self.us.map(|us| us / self.frames);
        log::info!(
            target: "iced_tiny_skia::cpu_profile",
            "iced: CPU frame profile frames={} failed={} size={}x{} age_min={} age_max={} \
             age_zero_frames={} full_region_frames={} empty_frames={} regions={} \
             damage_pixels_sum={} damage_pixels_max={} acquire_mean_us={} damage_mean_us={} \
             raster_mean_us={} present_mean_us={} total_mean_us={} total_max_us={}",
            self.frames, self.failed, self.width, self.height,
            self.age_min.unwrap_or(0), self.age_max, self.age_zero, self.full, self.empty,
            self.regions, self.pixels, self.pixels_max, acquire, damage, raster, present,
            self.total_us / self.frames, self.total_max_us,
        );
        *self = Self {
            since: Some(frame.phase),
            ..Self::default()
        };
    }
}
