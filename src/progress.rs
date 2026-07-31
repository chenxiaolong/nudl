// SPDX-FileCopyrightText: 2020-2026 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::VecDeque,
    fmt,
    io::{self, IoSlice, IsTerminal, Write},
    time::{Duration, Instant},
};

use anstyle_progress::TermProgress;
use indicatif::{BinaryBytes, MultiProgress, ProgressBar, ProgressState, style::ProgressTracker};
use tokio::sync::mpsc::{self, error::SendError};
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

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Osc94 {
    Hidden,
    Determinate(u8),
    Indeterminate,
}

#[derive(Clone)]
pub struct Osc94Printer {
    state: Osc94,
    supported: bool,
}

impl Osc94Printer {
    pub fn new() -> Self {
        Self {
            state: Osc94::Hidden,
            supported: anstyle_progress::supports_term_progress(io::stderr().is_terminal()),
        }
    }

    pub fn update(&mut self, state: Osc94) {
        if self.state != state {
            self.state = state;
            if self.supported {
                let term_progress = match state {
                    Osc94::Hidden => TermProgress::remove(),
                    Osc94::Determinate(p) => TermProgress::start().percent(p.min(100)),
                    Osc94::Indeterminate => TermProgress::start(),
                };

                // Max OSC 9;4 length is 13 bytes.
                let mut buf = [0u8; 16];
                let _ = write!(buf.as_mut_slice(), "{term_progress}");
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

pub const THROTTLE_DELAY: Duration = Duration::from_millis(50);

pub struct ThrottledProgress<T> {
    progress_tx: mpsc::Sender<T>,
    transform: fn(u64) -> T,
    delay: Duration,
    last_send: Instant,
    pending: u64,
}

impl<T> ThrottledProgress<T> {
    pub fn new(progress_tx: mpsc::Sender<T>, transform: fn(u64) -> T, delay: Duration) -> Self {
        let now = Instant::now();

        Self {
            progress_tx,
            transform,
            delay,
            last_send: now.checked_sub(delay).unwrap_or(now),
            pending: 0,
        }
    }

    async fn force_update(&mut self, inc: u64) -> Result<(), SendError<T>> {
        self.progress_tx
            .send((self.transform)(self.pending + inc))
            .await?;

        self.pending = 0;
        self.last_send = Instant::now();

        Ok(())
    }

    fn force_update_blocking(&mut self, inc: u64) -> Result<(), SendError<T>> {
        self.progress_tx
            .blocking_send((self.transform)(self.pending + inc))?;

        self.pending = 0;
        self.last_send = Instant::now();

        Ok(())
    }

    pub async fn update(&mut self, inc: u64) -> Result<(), SendError<T>> {
        if self.last_send.elapsed() >= self.delay {
            self.force_update(inc).await?;
        } else {
            self.pending += inc;
        }

        Ok(())
    }

    pub fn update_blocking(&mut self, inc: u64) -> Result<(), SendError<T>> {
        if self.last_send.elapsed() >= self.delay {
            self.force_update_blocking(inc)?;
        } else {
            self.pending += inc;
        }

        Ok(())
    }

    pub async fn flush(&mut self) -> Result<(), SendError<T>> {
        if self.pending > 0 {
            self.force_update(0).await?;
        }

        Ok(())
    }

    pub fn flush_blocking(&mut self) -> Result<(), SendError<T>> {
        if self.pending > 0 {
            self.force_update_blocking(0)?;
        }

        Ok(())
    }
}
