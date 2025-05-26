// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use cap_std::fs::{Dir, Metadata, OpenOptions};
use crc32fast::Hasher;
use tokio::{
    fs::File,
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::mpsc,
    task::{self, JoinSet},
    time,
};
use tokio_stream::StreamExt;
use tracing::{debug, trace, warn};
use zip::ZipArchive;
use zipunsplitlib::{
    file::{JoinedFile, MemoryCowFile, Opener},
    split,
};

use crate::client::{self, CarInfo, FirmwareInfo, NuClient};

const DOWNLOAD_EXT: &str = concat!(env!("CARGO_PKG_NAME"), "_download");
const EXTRACT_EXT: &str = concat!(env!("CARGO_PKG_NAME"), "_extract");
const VERIFY_EXT: &str = concat!(env!("CARGO_PKG_NAME"), "_verify");

const RETRY_DELAY: Duration = Duration::from_secs(1);

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

/// Delete a file, but don't error out if the path doesn't exist.
fn delete_if_exists(directory: &Dir, path: &Path) -> Result<()> {
    if let Err(e) = directory.remove_file(path) {
        if e.kind() != io::ErrorKind::NotFound {
            return Err(e).context(format!("Failed to delete file: {path:?}"));
        }
    }

    Ok(())
}

fn stat_if_exists(directory: &Dir, path: &Path) -> Result<Option<Metadata>> {
    match directory.metadata(path) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to stat: {path:?}")),
    }
}

#[derive(Clone, Copy, Debug)]
struct DownloadParams {
    file_index: usize,
    download_index: u32,
    start_offset: u64,
}

#[derive(Clone, Copy, Debug)]
struct PostProcessParams {
    file_index: usize,
    clean_only: bool,
}

#[derive(Debug)]
struct InitialState {
    /// Number of bytes already downloaded. This is based on the sum of the
    /// raw download sizes.
    dl_bytes: u64,
    /// Number of bytes post processed. This is based on the sum of the final
    /// output file sizes.
    pp_bytes: u64,
    /// Number of remaining raw downloads by file.
    dl_remain: Vec<u32>,
    /// Complete set of remaining download tasks.
    dl_tasks: VecDeque<DownloadParams>,
    /// Initial set of post-processing tasks that can be immediately executed.
    /// As download tasks complete and the remaining download count for a file
    /// in `dl_remain` drops to zero, more post-processing tasks will be added.
    pp_tasks: VecDeque<PostProcessParams>,
}

enum TaskResult {
    Download((usize, u32, Result<()>)),
    PostProcess((usize, Result<()>)),
}

pub enum ProgressMessage {
    TotalDownload(u64),
    TotalPostProcess(u64),
    Download(u64),
    PostProcess(u64),
}

struct SubdirOpener {
    dir: Arc<Dir>,
    paths: Vec<PathBuf>,
}

impl Opener for SubdirOpener {
    fn open_split(&mut self, index: usize) -> io::Result<std::fs::File> {
        self.dir.open(&self.paths[index]).map(|f| f.into_std())
    }

    fn num_splits(&self) -> usize {
        self.paths.len()
    }
}

pub struct Downloader {
    directory: Arc<Dir>,
    client: Arc<NuClient>,
    car: Arc<CarInfo>,
    firmware: Arc<FirmwareInfo>,
    concurrency: usize,
    retries: u8,
    keep_raw: bool,
    progress_tx: mpsc::Sender<ProgressMessage>,
}

impl Downloader {
    pub fn new(
        directory: Dir,
        client: NuClient,
        car: CarInfo,
        firmware: FirmwareInfo,
        concurrency: usize,
        retries: u8,
        keep_raw: bool,
    ) -> (Self, mpsc::Receiver<ProgressMessage>) {
        let (progress_tx, progress_rx) = mpsc::channel(2 * concurrency);

        let result = Self {
            directory: Arc::new(directory),
            client: Arc::new(client),
            car: Arc::new(car),
            firmware: Arc::new(firmware),
            concurrency,
            retries,
            keep_raw,
            progress_tx,
        };

        (result, progress_rx)
    }

    /// Compute contents of version info file.
    fn version_file(car: &CarInfo, firmware: &FirmwareInfo) -> String {
        use std::fmt::Write;

        let mut result = String::new();

        writeln!(
            &mut result,
            "+|{}|{}|{}|{}|{}|1",
            firmware.update_version,
            car.version,
            car.brand(),
            car.id,
            car.mcode,
        )
        .unwrap();

        for file in &firmware.files {
            let mut directory = String::new();
            if let Some(name) = &file.directory {
                directory.push('\\');
                directory.push_str(&name.replace('/', "\\"));
            }

            writeln!(
                &mut result,
                "{}{directory}|{}|{}|{}|{}|1",
                car.id,
                file.name,
                file.version,
                file.crc32 as i32,
                // This is signed in the raw response, but unsigned here.
                file.size,
            )
            .unwrap();
        }

        result
    }

    /// Write version info file to [`CarInfo::id`]`.ver`.
    fn write_version_file(directory: &Dir, car: &CarInfo, firmware: &FirmwareInfo) -> Result<()> {
        let path = format!("{}.ver", car.id);
        let contents = Self::version_file(car, firmware);

        directory
            .write(&path, contents)
            .with_context(|| format!("Failed to write file: {path}"))
    }

    fn compute_initial_state(
        base_directory: Arc<Dir>,
        firmware: Arc<FirmwareInfo>,
        cancel_signal: &AtomicBool,
    ) -> Result<InitialState> {
        let mut dl_bytes = 0;
        let mut pp_bytes = 0;
        let mut dl_remain = vec![0u32; firmware.files.len()];
        let mut dl_tasks = VecDeque::new();
        let mut pp_tasks = VecDeque::new();

        for (f_i, file_info) in firmware.files.iter().enumerate() {
            check_cancel(cancel_signal)?;

            let remain = &mut dl_remain[f_i];

            let owned_directory: Dir;
            let directory = if let Some(name) = &file_info.directory {
                match base_directory.open_dir(name) {
                    Ok(d) => {
                        owned_directory = d;
                        &owned_directory
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        // No output file or split exists.
                        for dl_i in 0..file_info.download_count() {
                            dl_tasks.push_back(DownloadParams {
                                file_index: f_i,
                                download_index: dl_i,
                                start_offset: 0,
                            });

                            *remain += 1;
                        }
                        continue;
                    }
                    Err(e) => {
                        return Err(e).with_context(|| format!("Failed to open directory: {name}"));
                    }
                }
            } else {
                &base_directory
            };

            if stat_if_exists(directory, Path::new(&file_info.name))?.is_some() {
                // Downloaded and post-processed.
                dl_bytes += file_info.download_size();
                pp_bytes += file_info.size;

                // Make sure splits are cleaned up.
                if file_info.is_split() {
                    pp_tasks.push_back(PostProcessParams {
                        file_index: f_i,
                        clean_only: true,
                    });
                }

                continue;
            }

            for dl_i in 0..file_info.download_count() {
                check_cancel(cancel_signal)?;

                // Completed raw download.
                let path = file_info.download_name(dl_i);
                if let Some(m) = stat_if_exists(directory, Path::new(&path))? {
                    dl_bytes += m.len();
                    continue;
                }

                if !file_info.is_split() {
                    // Unverified, but completed raw download of unsplit file.
                    let verify_path = format!("{path}.{VERIFY_EXT}");
                    if let Some(m) = stat_if_exists(directory, Path::new(&verify_path))? {
                        dl_bytes += m.len();
                        continue;
                    }
                }

                // Incomplete raw download.
                let download_path = format!("{path}.{DOWNLOAD_EXT}");
                let download_size = stat_if_exists(directory, Path::new(&download_path))?
                    .map(|m| m.len())
                    .unwrap_or_default();

                dl_bytes += download_size;
                dl_tasks.push_back(DownloadParams {
                    file_index: f_i,
                    download_index: dl_i,
                    start_offset: download_size,
                });

                *remain += 1;
            }

            if *remain == 0 {
                pp_tasks.push_back(PostProcessParams {
                    file_index: f_i,
                    clean_only: false,
                });
            }
        }

        Ok(InitialState {
            dl_bytes,
            pp_bytes,
            dl_remain,
            dl_tasks,
            pp_tasks,
        })
    }

    /// Download a single raw file (eg. a split). The download begins at the
    /// current file offset of `file`. The file data and metadata will be synced
    /// to disk when complete.
    async fn download_raw_to_file(
        file: &mut File,
        client: Arc<NuClient>,
        firmware: Arc<FirmwareInfo>,
        file_index: usize,
        download_index: u32,
        progress_tx: mpsc::Sender<ProgressMessage>,
    ) -> Result<()> {
        let file_info = &firmware.files[file_index];
        let path = file_info.download_path(download_index);

        let start = file
            .stream_position()
            .await
            .context("Failed to get file position")?;
        debug!("[{path}] Downloading from offset: {start}");

        let mut stream = match client
            .download(&firmware, file_info, download_index, start)
            .await
        {
            Ok(s) => s,
            Err(client::Error::AlreadyComplete) => {
                debug!("[{path}] Download already complete");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        while let Some(data) = stream.next().await {
            let data = data?;
            trace!("[{path}] Received {} bytes", data.len());

            file.write_all(&data)
                .await
                .with_context(|| format!("Failed to write {} bytes", data.len()))?;

            progress_tx
                .send(ProgressMessage::Download(data.len() as u64))
                .await?;
        }

        file.sync_all().await.context("Failed to sync file")?;

        Ok(())
    }

    /// Download a single raw file to `directory`. If the temp file for the
    /// download already exists, then the download is resumed. When complete,
    /// the temp file is renamed to the target file name for split files or with
    /// the [`VERIFY_EXT`] extension for unsplit files. Thus, this function is
    /// not idempotent.
    #[allow(clippy::too_many_arguments)]
    async fn download_raw(
        base_directory: Arc<Dir>,
        client: Arc<NuClient>,
        firmware: Arc<FirmwareInfo>,
        file_index: usize,
        download_index: u32,
        start: u64,
        retries: u8,
        progress_tx: mpsc::Sender<ProgressMessage>,
    ) -> Result<()> {
        let file_info = &firmware.files[file_index];
        let path = file_info.download_name(download_index);
        let download_path = format!("{path}.{DOWNLOAD_EXT}");

        let directory = if let Some(name) = &file_info.directory {
            task::spawn_blocking({
                let name = name.clone();

                move || {
                    base_directory
                        .create_dir_all(&name)
                        .with_context(|| format!("Failed to create directory: {name}"))?;

                    base_directory
                        .open_dir(&name)
                        .map(Arc::new)
                        .with_context(|| format!("Failed to open directory: {name}"))
                }
            })
            .await??
        } else {
            base_directory
        };

        let mut file = task::spawn_blocking({
            let directory = directory.clone();
            let download_path = download_path.clone();

            move || {
                directory.open_with(
                    &download_path,
                    OpenOptions::new().create(true).read(true).write(true),
                )
            }
        })
        .await?
        .map(|f| File::from_std(f.into_std()))
        .with_context(|| format!("Failed to open file: {download_path}"))?;

        file.seek(SeekFrom::Start(start))
            .await
            .with_context(|| format!("Failed to seek to {start}: {download_path}"))?;

        for attempt in 0..=retries {
            let ret = Self::download_raw_to_file(
                &mut file,
                client.clone(),
                firmware.clone(),
                file_index,
                download_index,
                progress_tx.clone(),
            )
            .await;

            match ret {
                Ok(_) => break,
                Err(e) if attempt == retries => {
                    return Err(e)
                        .with_context(|| format!("Failed to download to: {download_path}"));
                }
                Err(e) => {
                    warn!(
                        "[Attempt #{}/{}] Failed to download to: {download_path}: {e:?}",
                        attempt + 1,
                        u16::from(retries) + 1,
                    );
                    time::sleep(RETRY_DELAY).await;
                }
            }
        }

        drop(file);

        let rename_path = if file_info.is_split() {
            path
        } else {
            format!("{path}.{VERIFY_EXT}")
        };

        task::block_in_place(|| directory.rename(&download_path, &directory, &rename_path))
            .with_context(|| format!("Failed to move file: {download_path} -> {rename_path}"))?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_task(
        base_directory: Arc<Dir>,
        client: Arc<NuClient>,
        firmware: Arc<FirmwareInfo>,
        file_index: usize,
        download_index: u32,
        start: u64,
        retries: u8,
        progress_tx: mpsc::Sender<ProgressMessage>,
    ) -> TaskResult {
        let result = Self::download_raw(
            base_directory,
            client,
            firmware,
            file_index,
            download_index,
            start,
            retries,
            progress_tx,
        )
        .await;
        TaskResult::Download((file_index, download_index, result))
    }

    fn verify(
        directory: &Dir,
        firmware: &FirmwareInfo,
        file_index: usize,
        progress_tx: mpsc::Sender<ProgressMessage>,
        cancel_signal: &AtomicBool,
    ) -> Result<()> {
        let file_info = &firmware.files[file_index];
        assert!(!file_info.is_split(), "#{file_index} is a split file");

        let path = file_info.download_name(0);
        let verify_path = format!("{path}.{VERIFY_EXT}");

        let mut file = directory
            .open(&verify_path)
            .with_context(|| format!("Failed to open file: {verify_path}"))?;

        let mut hasher = Hasher::new();
        let mut buf = [0u8; 8192];

        loop {
            check_cancel(cancel_signal)?;

            let n = file
                .read(&mut buf)
                .with_context(|| format!("Failed to read file: {verify_path}"))?;
            if n == 0 {
                break;
            }

            hasher.update(&buf[..n]);

            progress_tx.blocking_send(ProgressMessage::PostProcess(n as u64))?;
        }

        let digest = hasher.finalize();
        if digest != file_info.crc32 {
            bail!(
                "Expected CRC32 {:08X}, but have {digest:08X}: {}",
                file_info.crc32,
                file_info.path(),
            );
        }

        drop(file);

        directory
            .rename(&verify_path, directory, &file_info.name)
            .with_context(
                || format!("Failed to move file: {verify_path} -> {}", file_info.name,),
            )?;

        Ok(())
    }

    fn extract(
        directory: Arc<Dir>,
        firmware: &FirmwareInfo,
        file_index: usize,
        progress_tx: mpsc::Sender<ProgressMessage>,
        cancel_signal: &AtomicBool,
    ) -> Result<()> {
        let file_info = &firmware.files[file_index];
        assert!(file_info.is_split(), "#{file_index} is not a split file");

        // Split files use the ancient split zip mechanism from the DOS era and
        // there are basically no libraries or tools that support reading them.
        // Instead, we'll create a copy-on-write virtual file that presents them
        // as a single concatenated file and then fix the file offsets in memory
        // so that it looks like a regular zip.
        let opener = SubdirOpener {
            dir: directory.clone(),
            paths: (0..file_info.download_count())
                .map(|i| PathBuf::from(file_info.download_name(i)))
                .collect(),
        };
        let joined = JoinedFile::new(opener).context("Failed to add splits to joined view")?;

        let expected_size = file_info.download_size();
        let actual_size = joined.len();

        if actual_size != expected_size {
            bail!(
                "Expected pieces to total {expected_size} bytes, but have {actual_size} bytes: {}",
                file_info.path(),
            );
        }

        let split_ranges = joined.splits();
        let mut cow_file = MemoryCowFile::new(joined, 4096)?;
        split::fix_offsets(&mut cow_file, &split_ranges)
            .with_context(|| format!("Failed to fix split zip offsets: {}", file_info.path()))?;
        cow_file.rewind()?;

        check_cancel(cancel_signal)?;

        let mut zip = ZipArchive::new(cow_file)?;
        if zip.len() != 1 {
            bail!(
                "Expected only a single entry in split zip, but have {}: {}",
                zip.len(),
                file_info.path(),
            );
        }

        let mut entry = zip
            .by_name(&file_info.name)
            .with_context(|| format!("Failed to open zip entry: {}", file_info.name))?;

        // Only need to check the metadata field. ZipArchive verifies the actual
        // digest after reading to EOF.
        if entry.crc32() != file_info.crc32 {
            bail!(
                "Expected CRC32 {:08X}, but have {:08X}: {}",
                file_info.crc32,
                entry.crc32(),
                file_info.name,
            );
        }

        let extract_path = format!("{}.{EXTRACT_EXT}", file_info.name);
        let mut file = directory
            .create(&extract_path)
            .with_context(|| format!("Failed to create file: {extract_path}"))?;
        let mut buf = [0u8; 8192];

        loop {
            check_cancel(cancel_signal)?;

            let n = entry
                .read(&mut buf)
                .with_context(|| format!("Failed to read split files: {}", file_info.path()))?;
            if n == 0 {
                break;
            }

            file.write_all(&buf[..n])
                .with_context(|| format!("Failed to write data: {extract_path}"))?;

            progress_tx.blocking_send(ProgressMessage::PostProcess(n as u64))?;
        }

        check_cancel(cancel_signal)?;

        file.sync_all()
            .with_context(|| format!("Failed to sync data: {extract_path}"))?;

        drop(file);

        directory
            .rename(&extract_path, &directory, &file_info.name)
            .with_context(|| {
                format!("Failed to move file: {extract_path} -> {}", file_info.name)
            })?;

        Ok(())
    }

    fn clean(
        directory: &Dir,
        firmware: &FirmwareInfo,
        file_index: usize,
        keep_raw: bool,
        cancel_signal: &AtomicBool,
    ) -> Result<()> {
        let file_info = &firmware.files[file_index];
        assert!(file_info.is_split(), "#{file_index} is not a split file");

        if !keep_raw {
            for i in 0..file_info.download_count() {
                check_cancel(cancel_signal)?;

                let path = file_info.download_name(i);

                delete_if_exists(directory, Path::new(&path))?;
            }
        }

        Ok(())
    }

    async fn post_process(
        base_directory: Arc<Dir>,
        firmware: Arc<FirmwareInfo>,
        file_index: usize,
        keep_raw: bool,
        clean_only: bool,
        progress_tx: mpsc::Sender<ProgressMessage>,
    ) -> Result<()> {
        let cancel_on_drop = CancelOnDrop::new();
        let cancel_signal = cancel_on_drop.handle();

        task::spawn_blocking(move || {
            let file_info = &firmware.files[file_index];

            let directory = if let Some(name) = &file_info.directory {
                base_directory
                    .open_dir(name)
                    .map(Arc::new)
                    .with_context(|| format!("Failed to open directory: {name}"))?
            } else {
                base_directory
            };

            if file_info.is_split() {
                if !clean_only {
                    Self::extract(
                        directory.clone(),
                        &firmware,
                        file_index,
                        progress_tx,
                        &cancel_signal,
                    )?;
                }

                Self::clean(&directory, &firmware, file_index, keep_raw, &cancel_signal)
            } else {
                Self::verify(
                    &directory,
                    &firmware,
                    file_index,
                    progress_tx,
                    &cancel_signal,
                )
            }
        })
        .await??;

        Ok(())
    }

    async fn post_process_task(
        base_directory: Arc<Dir>,
        firmware: Arc<FirmwareInfo>,
        file_index: usize,
        keep_raw: bool,
        clean_only: bool,
        progress_tx: mpsc::Sender<ProgressMessage>,
    ) -> TaskResult {
        let result = Self::post_process(
            base_directory,
            firmware,
            file_index,
            keep_raw,
            clean_only,
            progress_tx,
        )
        .await;
        TaskResult::PostProcess((file_index, result))
    }

    pub async fn download(&self) -> Result<()> {
        // Write version info file. This is not cancellable because it's a
        // single write operation.
        task::spawn_blocking({
            let directory = self.directory.clone();
            let car = self.car.clone();
            let firmware = self.firmware.clone();

            move || Self::write_version_file(&directory, &car, &firmware)
        })
        .await??;

        let mut state = {
            let cancel_on_drop = CancelOnDrop::new();
            let cancel_signal = cancel_on_drop.handle();

            let base_directory = self.directory.clone();
            let firmware = self.firmware.clone();

            task::spawn_blocking(move || {
                Self::compute_initial_state(base_directory, firmware, &cancel_signal)
            })
            .await??
        };

        // Report initial progress.
        let dl_total = self.firmware.files.iter().map(|f| f.download_size()).sum();
        let pp_total = self.firmware.size;

        self.progress_tx
            .send(ProgressMessage::TotalDownload(dl_total))
            .await?;
        self.progress_tx
            .send(ProgressMessage::TotalPostProcess(pp_total))
            .await?;
        self.progress_tx
            .send(ProgressMessage::Download(state.dl_bytes))
            .await?;
        self.progress_tx
            .send(ProgressMessage::PostProcess(state.pp_bytes))
            .await?;

        let mut tasks = JoinSet::new();
        let mut dl_running = 0;
        let mut pp_running = 0;

        loop {
            while dl_running < self.concurrency {
                let Some(params) = state.dl_tasks.pop_front() else {
                    break;
                };

                debug!(
                    "[Download#{}:{}] Task starting",
                    params.file_index, params.download_index,
                );
                dl_running += 1;
                tasks.spawn(Self::download_task(
                    self.directory.clone(),
                    self.client.clone(),
                    self.firmware.clone(),
                    params.file_index,
                    params.download_index,
                    params.start_offset,
                    self.retries,
                    self.progress_tx.clone(),
                ));
            }

            while pp_running < self.concurrency {
                let Some(params) = state.pp_tasks.pop_front() else {
                    break;
                };

                debug!("[PostProcess#{}] Task starting", params.file_index);
                pp_running += 1;
                tasks.spawn(Self::post_process_task(
                    self.directory.clone(),
                    self.firmware.clone(),
                    params.file_index,
                    self.keep_raw,
                    params.clean_only,
                    self.progress_tx.clone(),
                ));
            }

            let task_result = match tasks.join_next().await {
                // All tasks exited.
                None => break,
                // Task panicked or cancelled.
                Some(Err(e)) => return Err(e).context("Unexpected panic in task"),
                // Task completed.
                Some(Ok(result)) => result,
            };

            match task_result {
                TaskResult::Download((f_i, dl_i, result)) => {
                    debug!("[Download#{f_i}:{dl_i}] Task completed");
                    dl_running -= 1;
                    result?;

                    state.dl_remain[f_i] -= 1;

                    // Begin post-processing if there's nothing left to download
                    // for this output file.
                    if state.dl_remain[f_i] == 0 {
                        debug!("[Download#{f_i}:{dl_i}] Queuing post-processing task");
                        state.pp_tasks.push_back(PostProcessParams {
                            file_index: f_i,
                            clean_only: false,
                        });
                    }
                }
                TaskResult::PostProcess((f_i, result)) => {
                    debug!("[PostProcess#{f_i}] Task completed");
                    pp_running -= 1;
                    result?;
                }
            }
        }

        Ok(())
    }
}
