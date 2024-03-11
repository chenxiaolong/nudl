// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    ops::Range,
};

use thiserror::Error;

/// Magic bytes for each central directory header.
const MAGIC_CD: &[u8; 4] = b"\x50\x4b\x01\x02";
/// Magic bytes for each local header.
const MAGIC_LOCAL: &[u8; 4] = b"\x50\x4b\x03\x04";
/// Magic bytes for the end of central directory record.
const MAGIC_EOCD: &[u8; 4] = b"\x50\x4b\x05\x06";
/// Magic bytes for the zip64 end of central directory record.
const MAGIC_EOCD64: &[u8; 4] = b"\x50\x4b\x06\x06";
/// Magic bytes for the zip64 end of central directory locator.
const MAGIC_EOCD64_LOCATOR: &[u8; 4] = b"\x50\x4b\x06\x07";
/// Magic bytes at the start of the first split. This is the same as the
/// optional data descriptor magic.
const MAGIC_SPLIT: &[u8; 4] = b"\x50\x4b\x07\x08";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid split zip magic: {0:?}")]
    InvalidSplitMagic([u8; 4]),
    #[error("EOCD not found")]
    EocdNotFound,
    #[error("EOCD truncated")]
    EocdTruncated,
    #[error("Invalid zip64 EOCD magic: {0:?}")]
    InvalidEocd64Magic([u8; 4]),
    #[error("Central directory entry #{0} truncated")]
    CdTruncated(u64),
    #[error("Invalid magic in central directory entry #{0}: {1:?}")]
    InvalidCdMagic(u64, [u8; 4]),
    #[error("Extra field in central directory entry #{0} truncated")]
    CdExtraTruncated(u64),
    #[error("Found extra padding after central directory entries")]
    CdExtraSuffix,
    #[error("Missing split zip disk: {0}")]
    MissingDisk(usize),
    #[error("Field is out of bounds: {0}")]
    OutOfBounds(&'static str),
    #[error("I/O error")]
    Io(#[from] io::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// Fix the header offsets in a split zip file that was naively concatenated.
/// The split points must be specified via `disk_ranges`. This is necessary to
/// allow this procedure to work with any arbitrary unencrypted zip, including
/// zip64 files.
///
/// There are basically no libraries and tools that handle split zips correctly.
/// Even the official Info-ZIP implementation fails to unzip or unsplit a well
/// formed set of split zip files.
pub fn fix_offsets<F: Read + Write + Seek>(mut file: F, disk_ranges: &[Range<u64>]) -> Result<()> {
    // Naming conventions:
    // - boffset: Offset relative to the start of a buffer
    // - doffset: Offset relative to the start of a disk
    // - foffset: Offset relative to the start of the concatenated file

    if disk_ranges.is_empty() {
        return Err(Error::MissingDisk(0));
    }

    let file_size = disk_ranges.last().unwrap().end;

    let mut magic = [0u8; 4];
    file.rewind()?;
    file.read_exact(&mut magic)?;

    if magic != *MAGIC_SPLIT {
        return if magic == *MAGIC_LOCAL {
            // Assume this is a well-formed unsplit zip file.
            Ok(())
        } else {
            Err(Error::InvalidSplitMagic(magic))
        };
    }

    // Wipe out the split magic. It's fine for there to be extra preceding data
    // as long as those bytes don't have special meaning for zip parsers.
    file.rewind()?;
    file.write_all(b"\0\0\0\0")?;

    // Find EOCD at the end of the file. 65535 to account for maximum comment
    // size, 22 for size of EOCD, and 20 for size of zip64 EOCD locator.
    let mut search_window = [0u8; 65535 + 22 + 20];
    let search_size = file_size.min(search_window.len() as u64);
    let search_window = &mut search_window[..search_size as usize];
    file.seek(SeekFrom::Start(file_size - search_size))?;
    file.read_exact(search_window)?;

    let Some(eocd_boffset) = search_window.windows(4).position(|w| w == MAGIC_EOCD) else {
        return Err(Error::EocdNotFound);
    };
    let (pre_eocd, eocd) = search_window.split_at_mut(eocd_boffset);
    if eocd.len() < 22 {
        return Err(Error::EocdTruncated);
    }

    let eocd_foffset = file_size - eocd.len() as u64;
    let mut cd_entries;
    let mut cd_size;
    let mut cd_foffset;

    // The legacy EOCD is always present, even on zip64 archives.
    {
        let cd32_disk = u16::from_le_bytes(eocd[4..6].try_into().unwrap());
        let cd32_disk_range = disk_ranges
            .get(usize::from(cd32_disk))
            .ok_or_else(|| Error::MissingDisk(cd32_disk.into()))?;
        let cd32_entries = u16::from_le_bytes(eocd[10..12].try_into().unwrap());
        let cd32_size = u32::from_le_bytes(eocd[12..16].try_into().unwrap());
        let cd32_doffset = u32::from_le_bytes(eocd[16..20].try_into().unwrap());
        let cd32_foffset = cd32_disk_range
            .start
            .checked_add(cd32_doffset.into())
            .ok_or_else(|| Error::OutOfBounds("cd32_foffset"))?;

        // Number of this disk.
        eocd[4..6].fill(0);
        // Disk where the central directory starts.
        eocd[6..8].fill(0);
        // Number of central directory entries in this disk.
        eocd[8..10].copy_from_slice(&cd32_entries.to_le_bytes());
        // Offset to central directory.
        eocd[16..20].copy_from_slice(&(cd32_foffset.min(u32::MAX.into()) as u32).to_le_bytes());

        // Write fixed EOCD.
        file.seek(SeekFrom::Start(eocd_foffset))?;
        file.write_all(eocd)?;

        cd_entries = u64::from(cd32_entries);
        cd_size = u64::from(cd32_size);
        cd_foffset = cd32_foffset;
    }

    // The zip64 EOCD locator is guaranteed to immediately precede the EOCD.
    if pre_eocd.len() >= 20 && &pre_eocd[pre_eocd.len() - 20..][..4] == MAGIC_EOCD64_LOCATOR {
        let (_, eocd64_loc) = pre_eocd.split_at_mut(pre_eocd.len() - 20);
        let eocd64_loc_foffset = eocd_foffset - 20;

        let eocd64_disk = u32::from_le_bytes(eocd64_loc[4..8].try_into().unwrap());
        let eocd64_disk_range = disk_ranges
            .get(eocd64_disk as usize)
            .ok_or_else(|| Error::MissingDisk(eocd64_disk as usize))?;
        let eocd64_doffset = u64::from_le_bytes(eocd64_loc[8..16].try_into().unwrap());
        let eocd64_foffset = eocd64_disk_range
            .start
            .checked_add(eocd64_doffset)
            .ok_or_else(|| Error::OutOfBounds("eocd64_foffset"))?;

        // Disk where the zip64 EOCD starts.
        eocd64_loc[4..8].fill(0);
        // Offset to zip64 EOCD.
        eocd64_loc[8..16].copy_from_slice(&eocd64_foffset.to_le_bytes());
        // Total number of disks.
        eocd64_loc[16..20].copy_from_slice(&1u32.to_le_bytes());

        // Write fixed zip64 EOCD locator.
        file.seek(SeekFrom::Start(eocd64_loc_foffset))?;
        file.write_all(eocd64_loc)?;

        // We don't care about the zip64 extensible data sector.
        let mut eocd64 = [0u8; 56];
        file.seek(SeekFrom::Start(eocd64_foffset))?;
        file.read_exact(&mut eocd64)?;

        if eocd64[0..4] != *MAGIC_EOCD64 {
            return Err(Error::InvalidEocd64Magic(eocd64[0..4].try_into().unwrap()));
        }

        let cd64_disk = u32::from_le_bytes(eocd64[20..24].try_into().unwrap());
        let cd64_disk_range = disk_ranges
            .get(cd64_disk as usize)
            .ok_or_else(|| Error::MissingDisk(cd64_disk as usize))?;
        let cd64_entries = u64::from_le_bytes(eocd64[32..40].try_into().unwrap());
        let cd64_size = u64::from_le_bytes(eocd64[40..48].try_into().unwrap());
        let cd64_doffset = u64::from_le_bytes(eocd64[48..56].try_into().unwrap());
        let cd64_foffset = cd64_disk_range
            .start
            .checked_add(cd64_doffset)
            .ok_or_else(|| Error::OutOfBounds("cd64_foffset"))?;

        // Number of this disk.
        eocd64[16..20].fill(0);
        // Disk where the central directory starts.
        eocd64[20..24].fill(0);
        // Number of central directory entries in this disk.
        eocd64[24..32].copy_from_slice(&cd64_entries.to_le_bytes());
        // Offset to central directory
        eocd64[48..56].copy_from_slice(&cd64_foffset.to_le_bytes());

        // Write fixed zip64 EOCD.
        file.seek(SeekFrom::Start(eocd64_foffset))?;
        file.write_all(&eocd64)?;

        cd_entries = cd64_entries;
        cd_size = cd64_size;
        cd_foffset = cd64_foffset;
    }

    // Read the central directory.
    let cd_usize = usize::try_from(cd_size).map_err(|_| Error::OutOfBounds("cd_usize"))?;
    let mut cd = vec![0u8; cd_usize];
    file.seek(SeekFrom::Start(cd_foffset))?;
    file.read_exact(&mut cd)?;

    let mut cd_buf = cd.as_mut_slice();

    for entry in 0..cd_entries {
        if cd_buf.len() < 46 {
            return Err(Error::CdTruncated(entry));
        } else if cd_buf[0..4] != *MAGIC_CD {
            return Err(Error::InvalidCdMagic(
                entry,
                cd_buf[0..4].try_into().unwrap(),
            ));
        }

        let filename_len = u16::from_le_bytes(cd_buf[28..30].try_into().unwrap());
        let extra_len = u16::from_le_bytes(cd_buf[30..32].try_into().unwrap());
        let comment_len = u16::from_le_bytes(cd_buf[32..34].try_into().unwrap());
        let entry_size =
            46usize + usize::from(filename_len) + usize::from(extra_len) + usize::from(comment_len);

        let entry32_disk = u16::from_le_bytes(cd_buf[34..36].try_into().unwrap());
        if entry32_disk != 0xffff {
            // These will only be valid for non-zip64 entries.
            let entry32_disk_range = disk_ranges
                .get(usize::from(entry32_disk))
                .ok_or_else(|| Error::MissingDisk(entry32_disk.into()))?;
            let entry32_lh_doffset = u32::from_le_bytes(cd_buf[42..46].try_into().unwrap());
            let entry32_lh_foffset = entry32_disk_range
                .start
                .checked_add(entry32_lh_doffset.into())
                .ok_or_else(|| Error::OutOfBounds("entry32_lh_foffset"))?;

            // Starting disk number.
            cd_buf[34..36].fill(0);
            // Local header offset.
            cd_buf[42..46]
                .copy_from_slice(&(entry32_lh_foffset.min(u32::MAX.into()) as u32).to_le_bytes());
        }

        // Try to update zip64 versions of these fields.
        let mut extra =
            &mut cd_buf[46usize + usize::from(filename_len)..][..usize::from(extra_len)];
        while !extra.is_empty() {
            if extra.len() < 4 {
                return Err(Error::CdExtraTruncated(entry));
            }

            let id = u16::from_le_bytes(extra[0..2].try_into().unwrap());
            let size = u16::from_le_bytes(extra[2..4].try_into().unwrap());
            let data = &mut extra[4..];

            if data.len() < usize::from(size) {
                return Err(Error::CdExtraTruncated(entry));
            }

            if id == 1 {
                if data.len() < 28 {
                    return Err(Error::CdExtraTruncated(entry));
                }

                let entry64_lh_doffset = u64::from_le_bytes(data[16..24].try_into().unwrap());
                let entry64_disk = u32::from_le_bytes(data[24..28].try_into().unwrap());
                let entry64_disk_range = disk_ranges
                    .get(entry64_disk as usize)
                    .ok_or_else(|| Error::MissingDisk(entry64_disk as usize))?;
                let entry64_lh_foffset = entry64_disk_range
                    .start
                    .checked_add(entry64_lh_doffset)
                    .ok_or_else(|| Error::OutOfBounds("entry64_lh_foffset"))?;

                // Local header offset.
                data[16..24].copy_from_slice(&entry64_lh_foffset.to_le_bytes());
                // Starting disk number.
                data[24..28].fill(0);
            }

            extra = &mut extra[4 + usize::from(size)..];
        }

        cd_buf = &mut cd_buf[entry_size..];
    }

    if !cd_buf.is_empty() {
        return Err(Error::CdExtraSuffix);
    }

    // Finally, write the fixed central directory.
    file.seek(SeekFrom::Start(cd_foffset))?;
    file.write_all(&cd)?;

    Ok(())
}
