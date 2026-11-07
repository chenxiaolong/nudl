// SPDX-FileCopyrightText: 2024-2026 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{path::PathBuf, str::FromStr};

use anyhow::bail;
use tracing::Level;
use winnow_args::{Args, Subcommand, ValueEnum, complete::Shell};

use crate::Selector;

const MAX_CONCURRENCY: u8 = 16;

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

impl Brand {
    pub fn as_code_str(self) -> &'static str {
        match self {
            Self::Hyundai => "HM",
            Self::Kia => "KM",
            Self::Genesis => "GN",
        }
    }

    pub fn as_pretty_str(self) -> &'static str {
        match self {
            Self::Hyundai => "Hyundai",
            Self::Kia => "Kia",
            Self::Genesis => "Genesis",
        }
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

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    JsonRaw,
}

#[derive(Debug, Args)]
pub struct FamilyGroup {
    /// Car brand.
    #[arg(short, long)]
    pub brand: Brand,

    /// Car region.
    ///
    /// This is autodetected (likely by GeoIP) if unspecified.
    #[arg(short, long)]
    pub region: Option<String>,
}

#[derive(Debug, Args)]
pub struct FirmwareSelectorGroup {
    /// Select firmware by car model ID.
    ///
    /// The same car model ID may be used for multiple variants, like HEV vs.
    /// PHEV. To disambiguate, use `--version`.
    #[arg(short, long)]
    pub model: Option<String>,

    /// Select firmware by marketing name.
    #[arg(short, long)]
    pub name: Option<String>,

    /// Select firmware by version number.
    #[arg(short, long)]
    pub version: Option<String>,
}

impl FirmwareSelectorGroup {
    pub fn to_selectors(&self) -> Vec<Selector> {
        let mut selectors = vec![];

        if let Some(model) = &self.model {
            selectors.push(Selector::Model(model.clone()));
        }
        if let Some(name) = &self.name {
            selectors.push(Selector::Name(name.clone()));
        }
        if let Some(version) = &self.version {
            selectors.push(Selector::Version(version.clone()));
        }

        selectors
    }
}

#[derive(Debug, Args)]
pub struct NetworkGroup {
    /// Ignore TLS certificate validation for HTTPS connections.
    #[arg(long)]
    pub ignore_tls_validation: bool,
}

/// Generate shell tab completion configs.
#[derive(Debug, Args)]
pub struct CompletionCli {
    /// The shell to generate completions for.
    #[arg(short, long, value_name = "SHELL")]
    pub shell: Shell,
}

/// List available firmware.
#[derive(Debug, Args)]
#[arg(
    example(
        "nudl list -b <BRAND>",
        header = "To list available models and firmware versions"
    ),
    example(
        "nudl download -b <BRAND> -m <MODEL>",
        header = "To download the firmware for a specific model"
    ),
    example(
        "nudl download --help",
        header = "For more information about download-related options"
    )
)]
pub struct ListCli {
    #[arg(flatten)]
    pub family: FamilyGroup,

    #[arg(flatten)]
    pub selector: FirmwareSelectorGroup,

    /// Data output format.
    ///
    /// `text`: Two columns with the model ID and firmware version.
    /// `json`: Normalized data from the server containing more information.
    /// `json-raw`: Raw data from the server.
    #[arg(short, long, default = "text")]
    pub output: OutputFormat,

    #[arg(flatten)]
    pub network: NetworkGroup,
}

/// Download firmware.
#[derive(Debug, Args)]
#[arg(
    example(
        "nudl download -b <BRAND> -m <MODEL> -o <OUTPUT DIR>",
        header = "To download the firmware for a specific model"
    ),
    example("nudl list -b <BRAND>", header = "To list the available models"),
    after_help = "\
If there are multiple firmware variants for a single model (eg. HEV vs. PHEV),
the command will fail and print out the list of available downloads as well as
the additional command line options needed to select a specific one."
)]
pub struct DownloadCli {
    #[arg(flatten)]
    pub family: FamilyGroup,

    #[arg(flatten)]
    pub selector: FirmwareSelectorGroup,

    /// Output directory.
    #[arg(short, long, default = ".")]
    pub output: PathBuf,

    /// Download and post-processing concurrency.
    ///
    /// The maximum concurrency allowed is 16.
    #[arg(short, long, from_str, default = "4")]
    pub concurrency: Concurrency,

    /// Maximum retries during download.
    #[arg(long, default = "3")]
    pub retries: u8,

    /// Keep raw unextracted files.
    #[arg(short, long)]
    pub keep_raw: bool,

    #[arg(flatten)]
    pub network: NetworkGroup,
}

/// Verify CRC32 of existing firmware.
#[derive(Debug, Args)]
#[arg(after_help = "\
This subcommand is normally not needed because checksums are automatically
verified during the download process. However, it can be useful for verifying
checksums after the files have been copied elsewhere, for example to a USB
drive.

The vehicle itself will also verify checksums and digital signatures, so there
is minimal risk of corrupted files causing issues.")]
pub struct VerifyCli {
    /// Firmware directory.
    #[arg(short, long, default = ".")]
    pub directory: PathBuf,

    /// Verification concurrency.
    ///
    /// The maximum concurrency allowed is 16.
    #[arg(short, long, from_str, default = "4")]
    pub concurrency: Concurrency,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Completion(CompletionCli),
    List(ListCli),
    Download(DownloadCli),
    Verify(VerifyCli),
}

#[derive(Debug, Args)]
#[arg(
    name = "nudl",
    version,
    example(
        "nudl <COMMAND> --help",
        header = "For more information about a subcommand"
    )
)]
pub struct Cli {
    #[arg(subcommand)]
    pub command: Command,

    /// Lowest log message severity to output.
    #[arg(long, global, value_name = "LEVEL", from_str, default = "info")]
    pub log_level: Level,
}
