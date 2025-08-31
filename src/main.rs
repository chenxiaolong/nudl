// SPDX-FileCopyrightText: 2024-2025 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

mod cli;
mod client;
mod constants;
mod crypto;
mod download;
mod model;
mod progress;

use std::{
    fmt::{self, Display, Write as _},
    io::{self, IsTerminal, Write},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use cap_std::{ambient_authority, fs::Dir};
use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use tokio::signal::ctrl_c;
use tracing::debug;

use crate::{
    cli::{Brand, Cli, Command, DownloadCli, ListCli, OutputFormat},
    client::{AutodetectedRegion, CarInfo, NuClient, NuClientBuilder},
    download::{Downloader, ProgressMessage},
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
    brand: Brand,
    region: Option<&str>,
    ignore_tls_validation: bool,
) -> Result<(NuClient, String, String)> {
    let client = NuClientBuilder::new()
        .ignore_tls_validation(ignore_tls_validation)
        .build()?;

    let region = match region {
        Some(r) => r.to_owned(),
        None => match client.get_region(brand.as_code_str()).await? {
            AutodetectedRegion::Valid(r) => r,
            AutodetectedRegion::Invalid(r) => {
                bail!(
                    "No data available for autodetected region: {r}. Please manually specify a region."
                );
            }
        },
    };
    let guid = client.get_guid(&region).await?;

    Ok((client, region, guid))
}

async fn list_subcommand(cli: &Cli, list_cli: &ListCli) -> Result<()> {
    let (client, region, guid) = prepare_client(
        list_cli.family.brand,
        list_cli.family.region.as_deref(),
        cli.ignore_tls_validation,
    )
    .await?;
    let brand = list_cli.family.brand.as_code_str();
    let mut stdout = io::stdout().lock();

    match list_cli.output {
        OutputFormat::Text => {
            const HEADING_MODEL: &str = "MODEL";
            const HEADING_NAME: &str = "NAME";
            const HEADING_VERSION: &str = "VERSION";

            let cars = client.get_cars(&region, &guid, brand).await?;
            let model_max_len = cars
                .iter()
                .map(|c| c.id.len())
                .max()
                .unwrap_or_default()
                .max(HEADING_MODEL.len());
            let name_max_len = cars
                .iter()
                .map(|c| c.name.len())
                .max()
                .unwrap_or_default()
                .max(HEADING_NAME.len());

            writeln!(
                stdout,
                "{HEADING_MODEL:model_width$} {HEADING_NAME:name_width$} {HEADING_VERSION}",
                model_width = model_max_len,
                name_width = name_max_len + 2,
            )?;

            for car in cars {
                writeln!(
                    stdout,
                    "{:id_width$} \"{}\"{:name_padding$} {}",
                    car.id,
                    car.name,
                    "",
                    car.version,
                    id_width = model_max_len,
                    name_padding = name_max_len - car.name.len(),
                )?;
            }
        }
        OutputFormat::Json => {
            let cars = client.get_cars(&region, &guid, brand).await?;

            serde_json::to_writer_pretty(&mut stdout, &cars)?;
            writeln!(stdout)?;
        }
        OutputFormat::JsonRaw => {
            let raw_data = client.get_cars_raw(&region, &guid, brand).await?;

            serde_json::to_writer_pretty(&mut stdout, &raw_data)?;
            writeln!(stdout)?;
        }
    }

    Ok(())
}

fn join(into_iter: impl IntoIterator<Item = impl Display>, sep: &str) -> String {
    use std::fmt::Write;

    let mut result = String::new();

    for (i, item) in into_iter.into_iter().enumerate() {
        if i > 0 {
            result.push_str(sep);
        }

        write!(result, "{item}").expect("Failed to allocate");
    }

    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selector {
    Model(String),
    Name(String),
    Version(String),
}

impl Selector {
    fn all_for_car(car: &CarInfo) -> Vec<Selector> {
        vec![
            Self::Model(car.id.clone()),
            Self::Name(car.name.clone()),
            Self::Version(car.version.clone()),
        ]
    }

    fn matches_car(&self, car: &CarInfo) -> bool {
        match self {
            Self::Model(m) => car.id == *m,
            Self::Name(n) => car.name == *n,
            Self::Version(v) => car.version == *v,
        }
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(m) => write!(f, "-m {m}"),
            Self::Name(n) => write!(f, "-n \"{n}\""),
            Self::Version(v) => write!(f, "-v {v}"),
        }
    }
}

async fn download_subcommand(
    cli: &Cli,
    download_cli: &DownloadCli,
    bars: MultiProgress,
) -> Result<()> {
    let (client, region, guid) = prepare_client(
        download_cli.family.brand,
        download_cli.family.region.as_deref(),
        cli.ignore_tls_validation,
    )
    .await?;

    let cars = client
        .get_cars(&region, &guid, download_cli.family.brand.as_code_str())
        .await?;
    let selectors = download_cli.selector.to_selectors();
    let candidates: Vec<_> = cars
        .iter()
        .filter(|c| selectors.iter().all(|s| s.matches_car(c)))
        .collect();
    let car = if candidates.is_empty() {
        let mut msg = format!(
            "No firmware versions found matching selector: {}",
            join(selectors.iter(), " "),
        );

        for selector in &selectors {
            let available: Vec<_> = cars.iter().filter(|c| selector.matches_car(c)).collect();
            if !available.is_empty() {
                writeln!(&mut msg, "\n\nAvailable options for just: {selector}")?;
            }

            for car in available {
                let disambiguation = Selector::all_for_car(car)
                    .into_iter()
                    .filter(|s| s != selector);

                write!(&mut msg, "\n  {}", join(disambiguation, " "))?;
            }
        }

        bail!(msg);
    } else if candidates.len() > 1 {
        let mut msg = format!(
            "Multiple firmware versions found matching selector: {}\n",
            join(selectors.iter(), " "),
        );
        msg.push_str("To disambiguate, rerun with one of the following:\n");

        for car in candidates {
            let disambiguation = Selector::all_for_car(car)
                .into_iter()
                .filter(|s| !selectors.contains(s));

            write!(&mut msg, "\n  {}", join(disambiguation, " "))?;
        }

        bail!(msg);
    } else {
        candidates[0]
    };

    let firmware = client.get_firmware_info(&region, car).await?;

    println!("ID: {}", car.id);
    println!("Region: {region}");
    println!("Brand: {}", car.brand());
    println!("Model: {}", car.name);
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let bars = MultiProgress::new();
    let stderr = ProgressSuspendingStderr::new(bars.clone());

    tracing_subscriber::fmt()
        .with_writer(stderr)
        .with_ansi(io::stderr().is_terminal())
        .with_max_level(cli.log_level)
        .init();

    debug!("Arguments: {cli:#?}");

    match &cli.command {
        Command::List(c) => list_subcommand(&cli, c).await,
        Command::Download(c) => download_subcommand(&cli, c, bars).await,
    }
}
