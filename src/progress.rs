// SPDX-FileCopyrightText: 2020-2026 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::VecDeque,
    fmt,
    io::{self, IoSlice, IsTerminal, Write},
    time::{Duration, Instant},
};

use indicatif::{BinaryBytes, MultiProgress, ProgressBar, ProgressState, style::ProgressTracker};
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
        if let (Some(f), Some(b)) = (self.buf.front(), self.buf.back())
            && f != b
        {
            return (b.1 - f.1) as f64 / (b.0 - f.0).as_secs_f64();
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

#[allow(unused)]
#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
pub enum Osc94 {
    Hidden = 0,
    Normal(u8) = 1,
    Error(u8) = 2,
    Indeterminate = 3,
    Warning(u8) = 4,
}

impl fmt::Display for Osc94 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SAFETY: Primitive representation.
        // https://doc.rust-lang.org/reference/items/enumerations.html#r-items.enum.discriminant.access-memory
        let state = unsafe { *(self as *const Self).cast::<u8>() };

        let progress = match self {
            Self::Hidden | Self::Indeterminate => 0,
            Self::Normal(p) | Self::Error(p) | Self::Warning(p) => 100.min(*p),
        };

        write!(f, "\x1b]9;4;{state};{progress}\x07")
    }
}

#[derive(Clone)]
pub struct Osc94Printer {
    state: Osc94,
    is_terminal: bool,
}

impl Osc94Printer {
    pub fn new() -> Self {
        Self {
            state: Osc94::Hidden,
            is_terminal: io::stderr().is_terminal(),
        }
    }

    pub fn update(&mut self, state: Osc94) {
        if self.state != state {
            self.state = state;
            if self.is_terminal {
                // Max OSC 9;4 length is 14 bytes.
                let mut buf = [0u8; 16];
                let _ = write!(buf.as_mut_slice(), "{state}");
                let n = buf.iter().position(|b| *b == 0).unwrap();

                // Ensure the write is atomic.
                let _ = io::stderr().write(&buf[..n]);
            }
        }
    }
}

impl Drop for Osc94Printer {
    fn drop(&mut self) {
        self.update(Osc94::Hidden);
    }
}

pub fn progress_percentage(bars: &[&ProgressBar]) -> u8 {
    let ratio_sum = bars
        .iter()
        .map(|b| {
            if let Some(l) = b.length()
                && l > 0
            {
                b.position() as f64 / l as f64
            } else if b.position() == 0 {
                0f64
            } else {
                1f64
            }
        })
        .sum::<f64>();

    (ratio_sum / bars.len() as f64 * 100f64).round() as u8
}
