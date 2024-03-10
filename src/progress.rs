// SPDX-FileCopyrightText: 2020-2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::VecDeque,
    fmt,
    io::{self, IoSlice, Write},
    time::{Duration, Instant},
};

use indicatif::{style::ProgressTracker, BinaryBytes, MultiProgress, ProgressState};
use tracing_subscriber::fmt::MakeWriter;

/// Type that receives progress values and buffers them to compute the average
/// progress progression speed over the specified period of time.
#[derive(Debug, Clone)]
pub struct SpeedTracker {
    /// Period of time to accumulate records.
    duration: Duration,
    /// Buffer containing progress records over the specified period of time.
    buf: VecDeque<(Instant, u64)>,
}

impl SpeedTracker {
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            buf: VecDeque::new(),
        }
    }

    /// Clear all recorded values.
    fn reset(&mut self) {
        self.buf.clear();
    }

    /// Record progress value to be used for the speed calculation.
    fn record_value(&mut self, value: u64) {
        // Hack to ignore initial jump. There's no way to easily call reset()
        // without clearing all other state in the progress bar.
        if value == 0 {
            return;
        }

        let now = Instant::now();
        self.buf.push_back((now, value));

        // Only keep enough records to represent self.duration amount of time
        let end = self
            .buf
            .iter()
            .position(|x| now - x.0 < self.duration)
            .and_then(|x| x.checked_sub(1));
        if let Some(v) = end {
            self.buf.drain(0..v);
        }
    }

    /// Get progress speed as the number of progress units per second.
    fn units_per_sec(&self) -> f64 {
        if let (Some(f), Some(b)) = (self.buf.front(), self.buf.back()) {
            if f != b {
                return (b.1 - f.1) as f64 / (b.0 - f.0).as_secs_f64();
            }
        }

        0.0
    }
}

impl ProgressTracker for SpeedTracker {
    fn clone_box(&self) -> Box<dyn ProgressTracker> {
        Box::new(self.clone())
    }

    fn tick(&mut self, state: &ProgressState, _: Instant) {
        self.record_value(state.pos());
    }

    fn reset(&mut self, _state: &ProgressState, _: Instant) {
        self.reset();
    }

    fn write(&self, _state: &ProgressState, w: &mut dyn fmt::Write) {
        write!(w, "{}/s", BinaryBytes(self.units_per_sec() as u64)).unwrap();
    }
}

#[derive(Clone)]
pub struct ProgressSuspendingStderr {
    bars: MultiProgress,
}

impl ProgressSuspendingStderr {
    pub fn new(bars: MultiProgress) -> Self {
        Self { bars }
    }
}

impl Write for ProgressSuspendingStderr {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bars.suspend(|| io::stderr().write(buf))
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.bars.suspend(|| io::stderr().write_vectored(bufs))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.bars.suspend(|| io::stderr().flush())
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.bars.suspend(|| io::stderr().write_all(buf))
    }

    fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> io::Result<()> {
        self.bars.suspend(|| io::stderr().write_fmt(args))
    }
}

impl<'a> MakeWriter<'a> for ProgressSuspendingStderr {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
