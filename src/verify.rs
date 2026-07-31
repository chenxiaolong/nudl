// SPDX-FileCopyrightText: 2026 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, atomic::AtomicBool},
};

use cap_std::fs::Dir;
use crc32fast::Hasher;
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, error::SendError},
    task::{self, JoinError, JoinSet},
};
use tracing::{debug, error};

use crate::{
    cancel::{CancelOnDrop, check_cancel},
    progress::{THROTTLE_DELAY, ThrottledProgress},
    version::{self, VersionEntry, VersionInfo},
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to list directory")]
    ListDir(#[source] io::Error),
    #[error("Firmware directory must contain exactly one .ver file")]
    InvalidVerFileCount,
    #[error("Invalid .ver file: {0:?}")]
    InvalidVerFile(String, #[source] version::Error),
    #[error("Failed to open file: {0:?}")]
    OpenFile(String, #[source] io::Error),
    #[error("Failed to read file: {0:?}")]
    ReadFile(String, #[source] io::Error),
    #[error("Expected size {expected}, but have {actual}: {path:?}")]
    InvalidSize {
        path: String,
        actual: u64,
        expected: u64,
    },
    #[error("Expected CRC32 {expected:08X}, but have {actual:08X}: {path:?}")]
    InvalidCrc32 {
        path: String,
        actual: u32,
        expected: u32,
    },
    #[error("Verification failed")]
    Failed,
    #[error(transparent)]
    Progress(SendError<ProgressMessage>),
    #[error(transparent)]
    Cancelled(io::Error),
    #[error(transparent)]
    Panic(JoinError),
}

type Result<T, E = Error> = std::result::Result<T, E>;

pub enum ProgressMessage {
    Total(u64),
    Progress(u64),
}

pub struct Verifier {
    directory: Arc<Dir>,
    concurrency: usize,
    progress_tx: mpsc::Sender<ProgressMessage>,
}

impl Verifier {
    pub fn new(directory: Dir, concurrency: usize) -> (Self, mpsc::Receiver<ProgressMessage>) {
        let (progress_tx, progress_rx) = mpsc::channel(2 * concurrency);

        let result = Self {
            directory: Arc::new(directory),
            concurrency,
            progress_tx,
        };

        (result, progress_rx)
    }

    fn read_version_file(directory: &Dir) -> Result<VersionInfo> {
        let mut ver_file = None;

        for entry in directory.entries().map_err(Error::ListDir)? {
            let entry = entry.map_err(Error::ListDir)?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };

            if name.ends_with(".ver") {
                if ver_file.is_some() {
                    return Err(Error::InvalidVerFileCount);
                }

                let contents = directory
                    .read_to_string(&name)
                    .map_err(|e| Error::ReadFile(name.to_owned(), e))?;

                ver_file = Some((name, contents));
            }
        }

        let (ver_name, ver_contents) = ver_file.ok_or_else(|| Error::InvalidVerFileCount)?;

        ver_contents
            .parse::<VersionInfo>()
            .map_err(|e| Error::InvalidVerFile(ver_name.to_owned(), e))
    }

    fn verify_entry(
        directory: &Dir,
        entry: &VersionEntry,
        progress_tx: mpsc::Sender<ProgressMessage>,
        cancel_signal: &AtomicBool,
    ) -> Result<()> {
        let path = entry.path();

        let mut file = directory
            .open(path.as_ref())
            .map_err(|e| Error::OpenFile(path.clone().into_owned(), e))?;

        let size = file
            .seek(SeekFrom::End(0))
            .and_then(|s| file.rewind().map(|_| s))
            .map_err(|e| Error::ReadFile(path.clone().into_owned(), e))?;

        if size != entry.size {
            return Err(Error::InvalidSize {
                path: path.into_owned(),
                actual: size,
                expected: entry.size,
            });
        }

        let mut hasher = Hasher::new();
        let mut buf = [0u8; 8192];

        let mut progress =
            ThrottledProgress::new(progress_tx, ProgressMessage::Progress, THROTTLE_DELAY);

        loop {
            check_cancel(cancel_signal).map_err(Error::Cancelled)?;

            let n = file
                .read(&mut buf)
                .map_err(|e| Error::ReadFile(path.clone().into_owned(), e))?;
            if n == 0 {
                break;
            }

            hasher.update(&buf[..n]);

            progress
                .update_blocking(n as u64)
                .map_err(Error::Progress)?;
        }

        progress.flush_blocking().map_err(Error::Progress)?;

        let digest = hasher.finalize();
        if digest != entry.crc32 {
            return Err(Error::InvalidCrc32 {
                path: path.into_owned(),
                actual: digest,
                expected: entry.crc32,
            });
        }

        Ok(())
    }

    async fn verify_entry_task(
        task_id: usize,
        directory: Arc<Dir>,
        entry: VersionEntry,
        progress_tx: mpsc::Sender<ProgressMessage>,
    ) -> (usize, Result<()>) {
        let cancel_on_drop = CancelOnDrop::new();
        let cancel_signal = cancel_on_drop.handle();

        let result = task::spawn_blocking(move || {
            Self::verify_entry(&directory, &entry, progress_tx, &cancel_signal)
        })
        .await
        .map_err(Error::Panic)
        .flatten();

        (task_id, result)
    }

    pub async fn verify(&self) -> Result<()> {
        // Read version info file. This is not cancellable because it's a
        // single read operation.
        let info = task::spawn_blocking({
            let directory = self.directory.clone();
            move || Self::read_version_file(&directory)
        })
        .await
        .map_err(Error::Panic)
        .flatten()?;

        let mut entries = VecDeque::from(info.entries);
        let total_size = entries.iter().map(|e| e.size).sum();

        // Report initial progress.
        self.progress_tx
            .send(ProgressMessage::Total(total_size))
            .await
            .map_err(Error::Progress)?;
        self.progress_tx
            .send(ProgressMessage::Progress(0))
            .await
            .map_err(Error::Progress)?;

        let mut tasks = JoinSet::new();
        let mut next_task_id = 0;
        let mut running = 0;
        let mut failed = 0;

        loop {
            while running < self.concurrency {
                let Some(entry) = entries.pop_front() else {
                    break;
                };

                let task_id = next_task_id;
                next_task_id += 1;

                debug!("[Verify#{task_id}] Task starting");
                running += 1;
                tasks.spawn(Self::verify_entry_task(
                    task_id,
                    self.directory.clone(),
                    entry,
                    self.progress_tx.clone(),
                ));
            }

            let (task_id, task_result) = match tasks.join_next().await {
                // All tasks exited.
                None => break,
                // Task panicked or cancelled.
                Some(Err(e)) => return Err(Error::Panic(e)),
                // Task completed.
                Some(Ok((id, result))) => (id, result),
            };

            debug!("[Verify#{task_id}] Task completed");
            running -= 1;

            if let Err(e) = task_result {
                error!("{:#}", anyhow::Error::from(e));
                failed += 1;
            }
        }

        if failed > 0 {
            return Err(Error::Failed);
        }

        Ok(())
    }
}
