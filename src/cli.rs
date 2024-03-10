// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{fmt, path::PathBuf, str::FromStr};

use anyhow::bail;
use clap::{Args, Parser, Subcommand, ValueEnum};
use tracing::Level;

const MAX_CONCURRENCY: u8 = 16;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_level(self) -> Level {
        match self {
            Self::Trace => Level::TRACE,
            Self::Debug => Level::DEBUG,
            Self::Info => Level::INFO,
            Self::Warn => Level::WARN,
            Self::Error => Level::ERROR,
        }
    }
}

impl Default for LogLevel {
    fn default() -> Self {
        Self::Info
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.to_possible_value().ok_or(fmt::Error)?.get_name())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Concurrency(pub u8);

impl FromStr for Concurrency {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let n: u8 = s.parse()?;
        if n == 0 {
            bail!("value cannot be 0");
        } else if n > MAX_CONCURRENCY {
            // Same limit as aria2 to avoid unintentional DoS
            bail!("concurrency too high (>{MAX_CONCURRENCY})");
        }

        Ok(Self(n))
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Brand {
    Hyundai,
    Kia,
    Genesis,
}

impl fmt::Display for Brand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::Hyundai => "HM",
            Self::Kia => "KM",
            Self::Genesis => "GN",
        };

        f.write_str(code)
    }
}

impl FromStr for Brand {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "HM" => Ok(Self::Hyundai),
            "KM" => Ok(Self::Kia),
            "GN" => Ok(Self::Genesis),
            m => Err(m.to_owned()),
        }
    }
}

#[derive(Debug, Args)]
pub struct FamilyGroup {
    /// Car brand.
    #[clap(short, long)]
    pub brand: Brand,

    /// Car region.
    ///
    /// This is an uppercase ISO country code. Autodetected if unspecified.
    #[clap(short, long)]
    pub region: Option<String>,
}

/// List available firmware.
#[derive(Debug, Parser)]
pub struct ListCli {
    #[command(flatten)]
    pub family: FamilyGroup,
}

/// Download firmware.
#[derive(Debug, Parser)]
pub struct DownloadCli {
    #[command(flatten)]
    pub family: FamilyGroup,

    /// Car model,
    #[clap(short, long)]
    pub model: String,

    /// Output directory.
    #[clap(short, long, value_parser, default_value = ".")]
    pub output: PathBuf,

    /// Download and post-processing concurrency.
    ///
    /// The maximum concurrency allowed is 16.
    #[clap(short, long, default_value = "4")]
    pub concurrency: Concurrency,

    /// Maximum retries during download.
    #[clap(long, default_value = "3")]
    pub retries: u8,

    /// Keep raw unextracted files.
    #[clap(short, long)]
    pub keep_raw: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    List(ListCli),
    Download(DownloadCli),
}

#[derive(Debug, Parser)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Lowest log message severity to output.
    #[arg(long, global = true, value_name = "LEVEL", default_value_t)]
    pub log_level: LogLevel,

    /// Ignore TLS validation for HTTPS connections
    ///
    /// By default, all HTTPS connections (eg. to FUS) will validate the TLS
    /// certificate against the system's CA trust store.
    #[clap(long, global = true)]
    pub ignore_tls_validation: bool,
}
