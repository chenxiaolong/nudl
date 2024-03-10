// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{self, Read, Seek, SeekFrom, Write};

use thiserror::Error;

/// Magic bytes for each central directory header.
const MAGIC_CD: &[u8; 4] = b"\x50\x4b\x01\x02";
/// Magic bytes for each local header.
const MAGIC_LOCAL: &[u8; 4] = b"\x50\x4b\x03\x04";
/// Magic bytes for the end of central directory record.
const MAGIC_EOCD: &[u8; 4] = b"\x50\x4b\x05\x06";
/// Magic bytes at the start of the first split. This is the same as the
/// optional data descriptor magic.
const MAGIC_SPLIT: &[u8; 4] = b"\x50\x4b\x07\x08";

#[derive(Debug, Error)]
pub enum Error {
    #[error("File is too large for a non-zip64 file")]
    FileTooLarge,
    #[error("Invalid split zip magic: {0:?}")]
    InvalidSplitMagic([u8; 4]),
    #[error("EOCD not found")]
    EocdNotFound,
    #[error("Central directory entry #{0} truncated")]
    CdTruncated(u16),
    #[error("Invalid magic in central directory entry #{0}: {1:?}")]
    InvalidCdMagic(u16, [u8; 4]),
    #[error("Found extra padding before central directory entries")]
    CdExtraPrefix,
    #[error("Found extra padding after central directory entries")]
    CdExtraSuffix,
    #[error("Invalid magic in local header entry #{0}: {1:?}")]
    InvalidLocalMagic(u16, [u8; 4]),
    #[error("I/O error")]
    Io(#[from] io::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// Fix the header offsets in a split zip file that was naively concatenated.
///
/// There are basically no libraries and tools that handle split zips correctly.
/// Even the official Info-ZIP implementation fails to unzip or unsplit a well
/// formed set of split zip files.
///
/// This implementation does not support zip64 because it is not used by the NU
/// firmware files.
pub fn fix_offsets<F: Read + Write + Seek>(mut file: F) -> Result<()> {
    let file_size = file.seek(SeekFrom::End(0))?;
    if file_size > u64::from(u32::MAX) {
        return Err(Error::FileTooLarge);
    }

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

    // Find EOCD at the end of the file.
    let search_size = file_size.min(65536 + 20);
    let mut search_window = vec![0u8; search_size as usize];
    file.seek(SeekFrom::Start(file_size - search_size))?;
    file.read_exact(&mut search_window)?;

    let Some(eocd_rel_offset) = search_window.windows(4).position(|w| w == MAGIC_EOCD) else {
        return Err(Error::EocdNotFound);
    };
    let eocd = &mut search_window[eocd_rel_offset..];

    let cd_entries = u16::from_le_bytes(eocd[10..12].try_into().unwrap());
    let cd_size = u32::from_le_bytes(eocd[12..16].try_into().unwrap());
    let cd_offset = file_size - eocd.len() as u64 - u64::from(cd_size);

    // Number of this disk.
    eocd[4..6].fill(0);
    // Disk where the central directory starts.
    eocd[6..8].fill(0);
    // Number of central directory entries in this disk.
    eocd[8..10].copy_from_slice(&cd_entries.to_le_bytes());
    // Offset to central directory.
    eocd[16..20].copy_from_slice(&(cd_offset as u32).to_le_bytes());

    // Write fixed EOCD.
    file.seek(SeekFrom::Start(file_size - eocd.len() as u64))?;
    file.write_all(eocd)?;

    // Read the central directory.
    let mut cd = vec![0u8; cd_size as usize];
    file.seek(SeekFrom::Start(cd_offset))?;
    file.read_exact(&mut cd)?;

    let mut cd_entry_offset = 0;
    let mut cd_buf = cd.as_mut_slice();

    // Store the offsets to each local header offset field so that we can update
    // them later after parsing all the local headers.
    let mut entry_sizes = vec![];
    let mut lho_offsets = vec![];

    for entry in 0..cd_entries {
        if cd_buf.len() < 46 {
            return Err(Error::CdTruncated(entry));
        } else if cd_buf[0..4] != *MAGIC_CD {
            return Err(Error::InvalidCdMagic(
                entry,
                cd_buf[0..4].try_into().unwrap(),
            ));
        }

        let compressed_len = u32::from_le_bytes(cd_buf[20..24].try_into().unwrap());
        let filename_len = u16::from_le_bytes(cd_buf[28..30].try_into().unwrap());
        let extra_len = u16::from_le_bytes(cd_buf[30..32].try_into().unwrap());
        let comment_len = u16::from_le_bytes(cd_buf[32..34].try_into().unwrap());
        let cd_entry_size =
            46usize + usize::from(filename_len) + usize::from(extra_len) + usize::from(comment_len);

        entry_sizes.push(compressed_len);
        lho_offsets.push(cd_entry_offset + 42);

        // Starting disk number.
        cd_buf[34..36].fill(0);

        cd_entry_offset += cd_entry_size;
        cd_buf = &mut cd_buf[cd_entry_size..];
    }

    if !cd_buf.is_empty() {
        return Err(Error::CdExtraSuffix);
    }

    // Now, compute the actual local header offsets.
    let mut local_offset = file.seek(SeekFrom::Start(4))?;

    for entry in 0..cd_entries {
        let mut header = [0u8; 30];
        file.read_exact(&mut header)?;

        if header[0..4] != *MAGIC_LOCAL {
            return Err(Error::InvalidLocalMagic(
                entry,
                header[0..4].try_into().unwrap(),
            ));
        }

        let flags = u16::from_le_bytes(header[6..8].try_into().unwrap());
        let filename_len = u16::from_le_bytes(header[26..28].try_into().unwrap());
        let extra_len = u16::from_le_bytes(header[28..30].try_into().unwrap());

        // Update the local header offset in the central directory.
        let lho_offset = lho_offsets[entry as usize];
        cd[lho_offset..][..4].copy_from_slice(&(local_offset as u32).to_le_bytes());

        // Skip to next local header.
        let compressed_len = entry_sizes[entry as usize];
        local_offset = file.seek(SeekFrom::Current(
            i64::from(filename_len) + i64::from(extra_len) + i64::from(compressed_len),
        ))?;

        // Need to also skip data descriptor if this is a streaming zip.
        if flags & (1 << 3) != 0 {
            let mut magic = [0u8; 4];
            file.read_exact(&mut magic)?;

            // Data descriptor magic is optional.
            let skip_len = if magic == *MAGIC_SPLIT { 12 } else { 8 };

            local_offset = file.seek(SeekFrom::Current(skip_len))?;
        }
    }

    if local_offset != cd_offset {
        return Err(Error::CdExtraPrefix);
    }

    // Finally, write the fixed central directory.
    file.seek(SeekFrom::Start(cd_offset))?;
    file.write_all(&cd)?;

    Ok(())
}
