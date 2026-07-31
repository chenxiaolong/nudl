// SPDX-FileCopyrightText: 2024-2026 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

mod cancel;
mod cli;
mod client;
mod constants;
mod crypto;
mod download;
mod model;
mod progress;
mod verify;
mod version;

use std::{
    fmt::{self, Display, Write as _},
    io::{self, IsTerminal, Write},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use cap_std::{ambient_authority, fs::Dir};
use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use tokio::signal::ctrl_c;
use tracing::debug;
use unicode_width::UnicodeWidthStr;

use crate::{
    cli::{Brand, Cli, Command, DownloadCli, ListCli, OutputFormat, VerifyCli},
    client::{CarInfo, NuClient, NuClientBuilder},
    download::Downloader,
    progress::{Osc94, Osc94Printer, ProgressSuspendingStderr, SpeedTracker, progress_percentage},
    verify::Verifier,
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

    let (autodetected, region) = match region {
        Some(r) => (false, r.to_owned()),
        None => (true, client.get_region().await?),
    };

    if let Err(e) = client.validate_region(brand.as_code_str(), &region).await {
        return if autodetected {
            Err(e).context("Could not autodetect the region. Please manually specify a region.")
        } else {
            Err(e.into())
        };
    }

    let guid = client.get_guid(&region).await?;

    Ok((client, region, guid))
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
    fn all_for_car(car: &CarInfo) -> Vec<Self> {
        let mut result = vec![Self::Model(car.id.clone()), Self::Name(car.name.clone())];

        result.extend(car.versions.iter().cloned().map(Self::Version));

        result
    }

    fn matches_car(&self, car: &CarInfo) -> bool {
        match self {
            Self::Model(m) => car.id == *m,
            Self::Name(n) => car.name == *n,
            Self::Version(v) => car.versions.contains(v),
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

async fn list_subcommand(cli: &ListCli) -> Result<()> {
    let (client, region, guid) = prepare_client(
        cli.family.brand,
        cli.family.region.as_deref(),
        cli.network.ignore_tls_validation,
    )
    .await?;
    let brand = cli.family.brand.as_code_str();
    let selectors = cli.selector.to_selectors();

    match cli.output {
        OutputFormat::Text => {
            const HEADING_MODEL: &str = "MODEL";
            const HEADING_NAME: &str = "NAME";
            const HEADING_VERSION: &str = "VERSION";

            let mut cars = client.get_cars(&region, &guid, brand).await?;
            if !selectors.is_empty() {
                cars.retain(|c| selectors.iter().all(|s| s.matches_car(c)));
            }

            let model_max_width = cars
                .iter()
                .map(|c| c.id.width())
                .max()
                .unwrap_or_default()
                .max(HEADING_MODEL.width());
            let name_max_width = cars
                .iter()
                .map(|c| c.name.width())
                .max()
                .unwrap_or_default()
                .max(HEADING_NAME.width());

            let mut stdout = io::stdout().lock();

            writeln!(
                stdout,
                "{HEADING_MODEL:model_width$} {HEADING_NAME:name_width$} {HEADING_VERSION}",
                model_width = model_max_width,
                name_width = name_max_width + 2,
            )?;

            for car in cars {
                for version in &car.versions {
                    writeln!(
                        stdout,
                        "{:id_width$} \"{}\"{:name_padding$} {version}",
                        car.id,
                        car.name,
                        "",
                        id_width = model_max_width,
                        name_padding = name_max_width - car.name.width(),
                    )?;
                }
            }
        }
        OutputFormat::Json => {
            let mut cars = client.get_cars(&region, &guid, brand).await?;
            if !selectors.is_empty() {
                cars.retain(|c| selectors.iter().all(|s| s.matches_car(c)));
            }

            let mut stdout = io::stdout().lock();

            serde_json::to_writer_pretty(&mut stdout, &cars)?;
            writeln!(stdout)?;
        }
        OutputFormat::JsonRaw => {
            let raw_data = client.get_cars_raw(&region, &guid, brand).await?;
            if !selectors.is_empty() {
                bail!(
                    "Raw JSON output cannot be used with: {}",
                    join(selectors, " "),
                );
            }

            let mut stdout = io::stdout().lock();

            serde_json::to_writer_pretty(&mut stdout, &raw_data)?;
            writeln!(stdout)?;
        }
    }

    Ok(())
}

async fn download_subcommand(cli: &DownloadCli, bars: MultiProgress) -> Result<()> {
    let (client, region, guid) = prepare_client(
        cli.family.brand,
        cli.family.region.as_deref(),
        cli.network.ignore_tls_validation,
    )
    .await?;

    let cars = client
        .get_cars(&region, &guid, cli.family.brand.as_code_str())
        .await?;
    let selectors = cli.selector.to_selectors();
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
    println!("Brand: {}", car.brand.as_code_str());
    println!("Model: {}", car.name);
    println!("Version: {}", join(&car.versions, ", "));
    println!("Size: {} bytes", firmware.size);
    println!("Files:");

    for file in &firmware.files {
        println!("  {}", file.path());
        println!("    CRC32: {:08X}", file.crc32);
        println!("    Size: {} bytes", file.size);
    }

    let authority = ambient_authority();
    Dir::create_ambient_dir_all(&cli.output, authority)
        .with_context(|| format!("Failed to create directory: {:?}", cli.output))?;
    let directory = Dir::open_ambient_dir(&cli.output, authority)
        .with_context(|| format!("Failed to open directory: {:?}", cli.output))?;

    // The progress will be misreported if files are modified by external
    // processes. Solving this requires sending HEAD requests for each split and
    // preopening all files, which is inefficient and not worth doing. The files
    // would be corrupt anyway and the progress bar is the least of the user's
    // concerns.

    let mut osc94 = Osc94Printer::new();
    osc94.update(Osc94::Indeterminate);

    let p_dl = bars.add(ProgressBar::hidden());
    p_dl.set_prefix("Download");
    p_dl.set_style(progress_style());

    let p_pp = bars.add(ProgressBar::hidden());
    p_pp.set_prefix("Post-process");
    p_pp.set_style(progress_style());

    let (downloader, mut p_rx) = Downloader::new(
        directory,
        client,
        car.clone(),
        firmware,
        cli.concurrency.0.into(),
        cli.retries,
        cli.keep_raw,
    );
    let handle = downloader.download();
    tokio::pin!(handle);

    loop {
        tokio::select! {
            biased;

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
                        download::ProgressMessage::TotalDownload(bytes) => {
                            p_dl.set_length(bytes);
                        }
                        download::ProgressMessage::TotalPostProcess(bytes) => {
                            p_pp.set_length(bytes);
                        }
                        download::ProgressMessage::Download(bytes) => {
                            p_dl.inc(bytes);
                        }
                        download::ProgressMessage::PostProcess(bytes) => {
                            p_pp.inc(bytes);
                        }
                    }

                    let osc94_bars: &[&ProgressBar] = if p_pp.length() == Some(0) {
                        // If the server won't tell us the total extracted size,
                        // then don't include post-processing progress since
                        // otherwise the OSC 9;4 progress will always be more
                        // than halfway complete.
                        &[&p_dl]
                    } else {
                        &[&p_dl, &p_pp]
                    };

                    osc94.update(Osc94::Determinate(progress_percentage(osc94_bars)));
                }
            }
        }
    }

    Ok(())
}

async fn verify_subcommand(cli: &VerifyCli, bars: MultiProgress) -> Result<()> {
    let authority = ambient_authority();
    let directory = Dir::open_ambient_dir(&cli.directory, authority)
        .with_context(|| format!("Failed to open directory: {:?}", cli.directory))?;

    let mut osc94 = Osc94Printer::new();
    osc94.update(Osc94::Indeterminate);

    let p_verify = bars.add(ProgressBar::hidden());
    p_verify.set_prefix("Verify");
    p_verify.set_style(progress_style());

    let (verifier, mut p_rx) = Verifier::new(directory, cli.concurrency.0.into());
    let handle = verifier.verify();
    tokio::pin!(handle);

    loop {
        tokio::select! {
            biased;

            c = ctrl_c() => {
                let _ = bars.clear();
                c?;

                bail!("Verification was interrupted");
            }
            r = &mut handle => {
                let _ = bars.clear();
                r?;
                break;
            }
            p = p_rx.recv() => {
                if let Some(msg) = p {
                    match msg {
                        verify::ProgressMessage::Total(bytes) => {
                            p_verify.set_length(bytes);
                        }
                        verify::ProgressMessage::Progress(bytes) => {
                            p_verify.inc(bytes);
                        }
                    }

                    osc94.update(Osc94::Determinate(progress_percentage(&[&p_verify])));
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

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow!("Failed to set up ring as rustls crypto provider"))?;

    match &cli.command {
        Command::List(c) => list_subcommand(c).await,
        Command::Download(c) => download_subcommand(c, bars).await,
        Command::Verify(c) => verify_subcommand(c, bars).await,
    }
}
