// SPDX-FileCopyrightText: 2024-2026 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct CancelOnDrop(Arc<AtomicBool>);

impl CancelOnDrop {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn handle(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Returns an I/O error with the [`io::ErrorKind::Interrupted`] type if
/// `cancel_signal` is true. This should be called frequently in I/O loops for
/// cancellation to be responsive.
#[inline]
pub fn check_cancel(cancel_signal: &AtomicBool) -> io::Result<()> {
    if cancel_signal.load(Ordering::SeqCst) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Received cancel signal",
        ));
    }

    Ok(())
}
