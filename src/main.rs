// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

mod cli;
mod client;
mod constants;
mod crypto;
mod download;
mod file;
mod model;
mod progress;
mod split;

use std::{
    collections::HashMap,
    fs::File,
    io::{self, IsTerminal, Read, Seek, Write},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use cap_std::{ambient_authority, fs::Dir};
use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use tokio::{signal::ctrl_c, sync::mpsc, task};
use tracing::debug;

use crate::{
    cli::{Cli, Command, DownloadCli, JoinZipCli, ListCli},
    client::{NuClient, NuClientBuilder},
    download::{check_cancel, CancelOnDrop, Downloader, ProgressMessage},
    file::{JoinedFile, MemoryCowFile},
    progress::{ProgressSuspendingStderr, SpeedTracker},
};

const PROGRESS_SPEED_WINDOW: Duration = Duration::from_secs(1);

fn progress_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.green} {prefix}▕{wide_bar:.cyan/blue}▏{bytes}/{total_bytes} ({speed})",
    )
    .unwrap()
    .with_key("speed", SpeedTracker::new(PROGRESS_SPEED_WINDOW))
    .progress_chars("█▉▊▋▌▍▎▏  ")
}

async fn prepare_client(
    region: Option<&str>,
    ignore_tls_validation: bool,
) -> Result<(NuClient, String, String)> {
    let client = NuClientBuilder::new()
        .ignore_tls_validation(ignore_tls_validation)
        .build()?;

    let region = match region {
        Some(r) => r.to_owned(),
        None => client.get_region().await?,
    };
    let guid = client.get_guid(&region).await?;

    Ok((client, region, guid))
}

async fn list_subcommand(cli: &Cli, list_cli: &ListCli) -> Result<()> {
    let (client, region, guid) =
        prepare_client(list_cli.family.region.as_deref(), cli.ignore_tls_validation).await?;

    let cars = client
        .get_cars(&region, &guid, &list_cli.family.brand.to_string())
        .await?;
    let max_len = cars.iter().map(|c| c.id.len()).max().unwrap_or_default();

    for car in cars {
        println!("{:width$} {}", car.id, car.version, width = max_len);
    }

    Ok(())
}

async fn download_subcommand(
    cli: &Cli,
    download_cli: &DownloadCli,
    bars: MultiProgress,
) -> Result<()> {
    let (client, region, guid) = prepare_client(
        download_cli.family.region.as_deref(),
        cli.ignore_tls_validation,
    )
    .await?;

    let cars = client
        .get_cars(&region, &guid, &download_cli.family.brand.to_string())
        .await?;
    let Some(car) = cars.iter().find(|c| c.id == download_cli.model) else {
        bail!("No firmware found for model: {}", download_cli.model);
    };
    let firmware = client.get_firmware_info(car).await?;

    println!("ID: {}", car.id);
    println!("Region: {region}");
    println!("Brand: {}", car.brand());
    println!("Model: {}", car.model);
    println!("Version: {}", car.version);
    println!("Size: {} bytes", firmware.size);
    println!("Files:");

    for file in &firmware.files {
        println!("  {}", file.path());
        println!("    CRC32: {:08X}", file.crc32);
        println!("    Size: {} bytes", file.size);
    }

    let authority = ambient_authority();
    Dir::create_ambient_dir_all(&download_cli.output, authority)
        .with_context(|| format!("Failed to create directory: {:?}", download_cli.output))?;
    let directory = Dir::open_ambient_dir(&download_cli.output, authority)
        .with_context(|| format!("Failed to open directory: {:?}", download_cli.output))?;

    // The progress will be misreported if files are modified by external
    // processes. Solving this requires sending HEAD requests for each split and
    // preopening all files, which is inefficient and not worth doing. The files
    // would be corrupt anyway and the progress bar is the least of the user's
    // concerns.

    let mut p_dl_current = 0;
    let p_dl = bars.add(ProgressBar::hidden());
    p_dl.set_prefix("Download");
    p_dl.set_style(progress_style());

    let mut p_pp_current = 0;
    let p_pp = bars.add(ProgressBar::hidden());
    p_pp.set_prefix("Post-process");
    p_pp.set_style(progress_style());

    let (downloader, mut p_rx) = Downloader::new(
        directory,
        client,
        car.clone(),
        firmware,
        download_cli.concurrency.0.into(),
        download_cli.retries,
        download_cli.keep_raw,
    );
    let handle = downloader.download();
    tokio::pin!(handle);

    loop {
        tokio::select! {
            c = ctrl_c() => {
                let _ = bars.clear();
                c?;

                bail!("Download was interrupted. To resume, rerun the current command.");
            }
            r = &mut handle => {
                let _ = bars.clear();
                r?;
                break;
            }
            p = p_rx.recv() => {
                if let Some(msg) = p {
                    match msg {
                        ProgressMessage::TotalDownload(bytes) => {
                            p_dl.set_length(bytes);
                        }
                        ProgressMessage::TotalPostProcess(bytes) => {
                            p_pp.set_length(bytes);
                        }
                        ProgressMessage::Download(bytes) => {
                            p_dl_current += bytes;
                            p_dl.set_position(p_dl_current);
                        }
                        ProgressMessage::PostProcess(bytes) => {
                            p_pp_current += bytes;
                            p_pp.set_position(p_pp_current);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

async fn join_zip_subcommand(join_cli: &JoinZipCli, bars: MultiProgress) -> Result<()> {
    enum Message {
        Total(u64),
        Advance(u64),
    }

    let cancel_on_drop = CancelOnDrop::new();
    let cancel_signal = cancel_on_drop.handle();

    let inputs = join_cli.input.clone();
    let output = join_cli.output.clone();

    let progress = bars.add(ProgressBar::hidden());
    progress.set_prefix("Join");
    progress.set_style(progress_style());

    let (progress_tx, mut progress_rx) = mpsc::channel(8);

    let mut handle = task::spawn_blocking(move || {
        let mut joined = JoinedFile::new();
        let mut directories = HashMap::new();
        let authority = ambient_authority();

        for input in &inputs {
            check_cancel(&cancel_signal)?;

            let parent = input.parent().unwrap_or_else(|| Path::new("."));
            let Some(name) = input.file_name() else {
                bail!("Invalid path: {input:?}");
            };

            if !directories.contains_key(parent) {
                let directory = Dir::open_ambient_dir(parent, authority)
                    .map(Arc::new)
                    .with_context(|| format!("Failed to open directory: {parent:?}"))?;
                directories.insert(parent, directory);
            }

            let directory = directories[parent].clone();

            joined
                .add_file(directory, Path::new(name))
                .with_context(|| format!("Failed to add to joined view: {input:?}"))?;
        }

        progress_tx.blocking_send(Message::Total(joined.len()))?;

        let split_ranges = joined.splits();
        let mut cow_file = MemoryCowFile::new(joined, 4096)?;
        split::fix_offsets(&mut cow_file, &split_ranges)
            .context("Failed to fix split zip offsets")?;
        cow_file.rewind()?;

        check_cancel(&cancel_signal)?;

        let mut file =
            File::create(&output).with_context(|| format!("Failed to create file: {output:?}"))?;
        let mut buf = [0u8; 8192];

        loop {
            check_cancel(&cancel_signal)?;

            let n = cow_file
                .read(&mut buf)
                .context("Failed to read split files")?;
            if n == 0 {
                break;
            }

            file.write_all(&buf[..n])
                .with_context(|| format!("Failed to write data: {output:?}"))?;

            progress_tx.blocking_send(Message::Advance(n as u64))?;
        }

        Ok(())
    });

    loop {
        tokio::select! {
            c = ctrl_c() => {
                let _ = bars.clear();
                c?;

                bail!("Interrupted.");
            }
            r = &mut handle => {
                let _ = bars.clear();
                r??;
                break;
            }
            p = progress_rx.recv() => {
                if let Some(msg) = p {
                    match msg {
                        Message::Total(bytes) => {
                            progress.set_length(bytes);
                        }
                        Message::Advance(bytes) => {
                            progress.set_position(progress.position() + bytes);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let bars = MultiProgress::new();
    let stderr = ProgressSuspendingStderr::new(bars.clone());

    tracing_subscriber::fmt()
        .with_writer(stderr)
        .with_ansi(io::stderr().is_terminal())
        .with_max_level(cli.log_level.as_level())
        .init();

    debug!("Arguments: {cli:#?}");

    match &cli.command {
        Command::List(c) => list_subcommand(&cli, c).await,
        Command::Download(c) => download_subcommand(&cli, c, bars).await,
        Command::JoinZip(c) => join_zip_subcommand(c, bars).await,
    }
}
