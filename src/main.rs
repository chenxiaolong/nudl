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
    io::{self, IsTerminal},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use cap_std::{ambient_authority, fs::Dir};
use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use tokio::signal::ctrl_c;
use tracing::debug;

use crate::{
    cli::{Cli, Command, DownloadCli, ListCli},
    client::{NuClient, NuClientBuilder},
    download::{Downloader, ProgressMessage},
    progress::{ProgressSuspendingStderr, SpeedTracker},
};

const PROGRESS_SPEED_WINDOW: Duration = Duration::from_secs(1);

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

    let p_style = ProgressStyle::with_template(
        "{spinner:.green} {prefix}▕{wide_bar:.cyan/blue}▏{bytes}/{total_bytes} ({speed})",
    )
    .unwrap()
    .with_key("speed", SpeedTracker::new(PROGRESS_SPEED_WINDOW))
    .progress_chars("█▉▊▋▌▍▎▏  ");

    // The progress will be misreported if files are modified by external
    // processes. Solving this requires sending HEAD requests for each split and
    // preopening all files, which is inefficient and not worth doing. The files
    // would be corrupt anyway and the progress bar is the least of the user's
    // concerns.

    let mut p_dl_current = 0;
    let p_dl = bars.add(ProgressBar::hidden());
    p_dl.set_prefix("Download");
    p_dl.set_style(p_style.clone());

    let mut p_pp_current = 0;
    let p_pp = bars.add(ProgressBar::hidden());
    p_pp.set_prefix("Post-process");
    p_pp.set_style(p_style);

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
    }
}
