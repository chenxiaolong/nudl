// SPDX-FileCopyrightText: 2024-2026 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{borrow::Cow, fmt, str::FromStr};

use thiserror::Error;

use crate::client::{BrandInfo, CarInfo, FirmwareInfo};

#[derive(Debug, Error)]
pub enum Error {
    #[error("Version file is empty")]
    EmptyFile,
    #[error("Malformed version header line: {0:?}")]
    MalformedHeader(String),
    #[error("Malformed version entry line: {0:?}")]
    MalformedEntry(String),
}

type Result<T, E = Error> = std::result::Result<T, E>;

pub struct VersionHeader {
    pub update_version: String,
    pub firmware_version: String,
    pub brand: BrandInfo,
    pub id: String,
    pub mcode: String,
}

impl fmt::Display for VersionHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "+|{}|{}|{}|{}|{}|1",
            self.update_version,
            self.firmware_version,
            self.brand.as_code_str(),
            self.id,
            self.mcode,
        )
    }
}

impl FromStr for VersionHeader {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        fn parse(s: &str) -> Option<VersionHeader> {
            let mut iter = s.split('|');

            let plus = iter.next()?;
            if plus != "+" {
                return None;
            }

            let update_version = iter.next()?;
            let firmware_version = iter.next()?;
            let brand = iter.next()?;
            let id = iter.next()?;
            let mcode = iter.next()?;

            let completed = iter.next()?;
            if completed != "1" {
                return None;
            }

            if iter.next().is_some() {
                return None;
            }

            Some(VersionHeader {
                update_version: update_version.to_owned(),
                firmware_version: firmware_version.to_owned(),
                brand: BrandInfo::new(brand),
                id: id.to_owned(),
                mcode: mcode.to_owned(),
            })
        }

        parse(s).ok_or_else(|| Error::MalformedHeader(s.to_owned()))
    }
}

pub struct VersionEntry {
    pub id: String,
    pub directory: Option<String>,
    pub filename: String,
    pub version: String,
    pub crc32: u32,
    pub size: u64,
}

impl VersionEntry {
    pub fn path(&self) -> Cow<'_, str> {
        match &self.directory {
            Some(d) => Cow::Owned(format!("{d}/{}", self.filename)),
            None => Cow::Borrowed(&self.filename),
        }
    }
}

impl fmt::Display for VersionEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id)?;

        if let Some(path) = &self.directory {
            for component in path.split('/') {
                f.write_str("\\")?;
                f.write_str(component)?;
            }
        }

        write!(
            f,
            "|{}|{}|{}|{}|1",
            self.filename, self.version, self.crc32 as i32, self.size,
        )
    }
}

impl FromStr for VersionEntry {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        fn parse(s: &str) -> Option<VersionEntry> {
            let mut iter = s.split('|');

            let id_directory = iter.next()?;
            let (id, directory) = match id_directory.split_once('\\') {
                Some((i, d)) => (i, Some(d.replace("\\", "/"))),
                None => (id_directory, None),
            };

            let filename = iter.next()?;
            let version = iter.next()?;
            let crc32 = iter.next()?.parse::<i32>().ok()? as u32;
            let size = iter.next()?.parse::<u64>().ok()?;

            let completed = iter.next()?;
            if completed != "1" {
                return None;
            }

            if iter.next().is_some() {
                return None;
            }

            Some(VersionEntry {
                id: id.to_owned(),
                directory,
                filename: filename.to_owned(),
                version: version.to_owned(),
                crc32,
                size,
            })
        }

        parse(s).ok_or_else(|| Error::MalformedEntry(s.to_owned()))
    }
}

pub struct VersionInfo {
    pub header: VersionHeader,
    pub entries: Vec<VersionEntry>,
}

impl VersionInfo {
    pub fn new(car: &CarInfo, firmware: &FirmwareInfo) -> Self {
        let header = VersionHeader {
            update_version: firmware.update_version.clone(),
            // The official app always puts the first listed version number in
            // this file. All output files are exactly identical regardless of
            // which firmware version the user selects for the same model ID.
            firmware_version: car.versions[0].clone(),
            brand: car.brand.clone(),
            id: car.id.clone(),
            mcode: car.mcode.clone(),
        };

        let mut entries = vec![];

        for file in &firmware.files {
            entries.push(VersionEntry {
                id: car.id.clone(),
                directory: file.directory.clone(),
                filename: file.name.clone(),
                version: file.version.clone(),
                crc32: file.crc32,
                size: file.size,
            });
        }

        Self { header, entries }
    }
}

impl fmt::Display for VersionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.header)?;

        for entry in &self.entries {
            writeln!(f, "{entry}")?;
        }

        Ok(())
    }
}

impl FromStr for VersionInfo {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut iter = s.split_terminator("\n");

        let header = iter
            .next()
            .ok_or(Error::EmptyFile)?
            .parse::<VersionHeader>()?;

        let mut entries = vec![];

        for line in iter {
            entries.push(line.parse::<VersionEntry>()?);
        }

        Ok(Self { header, entries })
    }
}
