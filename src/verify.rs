// SPDX-FileCopyrightText: 2025 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    io::{self, Read},
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};
use cap_std::fs::Dir;
use crc32fast::Hasher;

/// A single file entry parsed from a `.ver` checksum file.
#[derive(Clone, Debug)]
pub struct VerEntry {
    /// Path of the output file relative to the firmware directory.
    pub path: String,
    /// Expected CRC32 digest of the output file.
    pub crc32: u32,
    /// Expected size of the output file in bytes.
    pub size: u64,
}

/// Result of verifying a single file against its expected checksum.
#[derive(Debug)]
pub enum FileStatus {
    /// The file exists and its size and CRC32 both match.
    Ok,
    /// The file does not exist.
    Missing,
    /// The file exists but has a different size.
    SizeMismatch { expected: u64, actual: u64 },
    /// The file exists and has the expected size, but a different CRC32.
    CrcMismatch { expected: u32, actual: u32 },
}

/// Parse the contents of a `.ver` checksum file.
///
/// The file has one header line beginning with `+`, followed by one line per
/// file of the form:
///
/// ```text
/// <model id>\<dir>|<name>|<version>|<crc32>|<size>|1
/// ```
///
/// The `<crc32>` field is a signed 32-bit integer and the leading `<model id>`
/// path component is not a real directory on disk (the official client and this
/// tool both place files directly in the output directory), so it is stripped
/// to produce a path relative to the firmware directory.
pub fn parse_ver_file(contents: &str) -> Result<Vec<VerEntry>> {
    let mut entries = Vec::new();

    for (i, line) in contents.lines().enumerate() {
        // Skip empty lines and the header line.
        if line.is_empty() || line.starts_with('+') {
            continue;
        }

        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() < 6 {
            bail!("Malformed entry on line {}: {line:?}", i + 1);
        }

        let dir_field = fields[0];
        let name = fields[1];
        // Stored as a signed 32-bit integer, matching the server's raw value.
        let crc32 = fields[3]
            .parse::<i32>()
            .with_context(|| format!("Invalid CRC32 on line {}: {:?}", i + 1, fields[3]))?
            as u32;
        let size = fields[4]
            .parse::<u64>()
            .with_context(|| format!("Invalid size on line {}: {:?}", i + 1, fields[4]))?;

        // Normalize the Windows-style path and drop the leading model-id
        // component to get a path relative to the firmware directory.
        let normalized = dir_field.replace('\\', "/");
        let mut path = String::new();

        for component in normalized.split('/').skip(1) {
            if component.is_empty() {
                continue;
            }
            path.push_str(component);
            path.push('/');
        }
        path.push_str(name);

        entries.push(VerEntry { path, crc32, size });
    }

    if entries.is_empty() {
        bail!("No file entries found in .ver file");
    }

    Ok(entries)
}

/// Find the single `.ver` file in `directory`.
///
/// Returns an error if there are zero or multiple `.ver` files.
pub fn find_ver_file(directory: &Dir) -> Result<String> {
    let mut found = None;

    for entry in directory.entries().context("Failed to list directory")? {
        let entry = entry.context("Failed to read directory entry")?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if name.ends_with(".ver") {
            if found.is_some() {
                bail!("Multiple .ver files found; specify one with --ver-file");
            }
            found = Some(name.into_owned());
        }
    }

    found.ok_or_else(|| anyhow!("No .ver file found; specify one with --ver-file"))
}

/// Verify a single file entry against the file on disk.
///
/// `progress` is called with the number of bytes read after each chunk so the
/// caller can update a progress bar. This never fails on a mismatch; it only
/// returns an error for unexpected I/O failures.
pub fn verify_entry(
    directory: &Dir,
    entry: &VerEntry,
    mut progress: impl FnMut(u64),
) -> Result<FileStatus> {
    let mut file = match directory.open(Path::new(&entry.path)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FileStatus::Missing),
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to open file: {}", entry.path));
        }
    };

    let mut hasher = Hasher::new();
    let mut actual_size = 0u64;
    let mut buf = [0u8; 8192];

    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("Failed to read file: {}", entry.path))?;
        if n == 0 {
            break;
        }

        hasher.update(&buf[..n]);
        actual_size += n as u64;
        progress(n as u64);
    }

    if actual_size != entry.size {
        return Ok(FileStatus::SizeMismatch {
            expected: entry.size,
            actual: actual_size,
        });
    }

    let digest = hasher.finalize();
    if digest != entry.crc32 {
        return Ok(FileStatus::CrcMismatch {
            expected: entry.crc32,
            actual: digest,
        });
    }

    Ok(FileStatus::Ok)
}
