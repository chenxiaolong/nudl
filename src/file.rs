// SPDX-FileCopyrightText: 2024 Andrew Gunnerson
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    cmp::Ordering,
    collections::{hash_map::Entry, HashMap},
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_std::fs::Dir;

/// Read data from offset. The file position *will* be changed.
#[cfg(windows)]
pub fn read_at(file: &mut File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, offset)
}

/// Read data from offset. The file position will *not* be changed.
#[cfg(unix)]
pub fn read_at(file: &mut File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, offset)
}

/// Present a set of split files in a single joined read-only view.
///
/// The file size of each split is queried once during [`Self::add_file`]. Files
/// are not opened until they are needed for [`Self::read`]. If EOF occurs in a
/// split before the overall EOF is reached, [`Self::read`] will return an
/// error.
///
/// Note that a single [`Self::read()`] call will correspond to a single read
/// system call and thus, will not cross split boundaries.
pub struct JoinedFile {
    paths: Vec<(Arc<Dir>, PathBuf)>,
    splits: Vec<Range<u64>>,
    cur_split: Option<usize>,
    cur_file: Option<(usize, File)>,
    cur_offset: u64,
}

impl JoinedFile {
    pub fn new() -> Self {
        Self {
            paths: vec![],
            splits: vec![],
            cur_split: None,
            cur_file: None,
            cur_offset: 0,
        }
    }

    /// Get the joined length of all splits.
    pub fn len(&self) -> u64 {
        self.splits.last().map(|s| s.end).unwrap_or_default()
    }

    /// Get the boundaries for each split.
    pub fn splits(&self) -> Vec<Range<u64>> {
        self.splits.clone()
    }

    /// Add the next file split. The size of this split is queried once and then
    /// cached. This will change the total size of the joined view, which
    /// affects seeks relative to EOF.
    pub fn add_file(&mut self, directory: Arc<Dir>, path: &Path) -> io::Result<&mut Self> {
        let file = directory.open(path)?;
        let size = file.metadata()?.len();
        let prev_len = self.len();
        let cur_len = prev_len + size;

        self.paths.push((directory, path.to_owned()));
        self.splits.push(prev_len..cur_len);

        if self.cur_split.is_none() && self.cur_offset < cur_len {
            // cur_split is only ever None if cur_offset is past EOF. If adding
            // a file brings that back into bounds, then the offset must belong
            // to this newly added split.
            self.cur_split = Some(self.splits.len() - 1);
        }

        Ok(self)
    }

    fn ensure_opened(&mut self) -> io::Result<()> {
        let Some(cur_split) = self.cur_split else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "No split file selected",
            ));
        };

        if let Some((i, _)) = self.cur_file.as_mut() {
            if *i == cur_split {
                return Ok(());
            }
        }

        let (directory, path) = &self.paths[cur_split];
        let file = directory.open(path)?;

        self.cur_file = Some((cur_split, file.into_std()));

        Ok(())
    }
}

impl Read for JoinedFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let Some(cur_split) = self.cur_split else {
            return Ok(0);
        };

        self.ensure_opened()?;

        let (_, file) = self.cur_file.as_mut().unwrap();
        let range = &self.splits[cur_split];

        let to_read = (range.end - self.cur_offset).min(buf.len() as u64) as usize;

        let n = read_at(file, &mut buf[..to_read], self.cur_offset - range.start)?;
        if n == 0 {
            // We should never report EOF in the middle of the file.
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("Split #{cur_split} was truncated"),
            ));
        }

        self.cur_offset += n as u64;
        debug_assert!(
            self.cur_offset <= range.end,
            "Read more data than requested",
        );

        if self.cur_offset == range.end {
            // Split has been fully consumed.
            if cur_split + 1 == self.splits.len() {
                self.cur_split = None;
            } else {
                self.cur_split = Some(cur_split + 1);
            }
        }

        Ok(n)
    }
}

impl Seek for JoinedFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_offset = match pos {
            SeekFrom::Start(o) => o,
            SeekFrom::End(o) => self
                .len()
                .checked_add_signed(o)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Out of bounds"))?,
            SeekFrom::Current(o) => self
                .cur_offset
                .checked_add_signed(o)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Out of bounds"))?,
        };

        if self.cur_offset != new_offset {
            self.cur_offset = new_offset;
            self.cur_split = self
                .splits
                .binary_search_by(|range| {
                    if range.start > self.cur_offset {
                        Ordering::Greater
                    } else if range.end <= self.cur_offset {
                        Ordering::Less
                    } else {
                        Ordering::Equal
                    }
                })
                .ok();
        }

        Ok(self.cur_offset)
    }
}

/// An in-memory copy-on-write wrapper around a reader. This allows modifying
/// the data without affecting the underlying reader. Writing beyond the end of
/// the source file is permitted.
///
/// Note that a single [`Self::read()`] call will not cross a block boundary
/// where the block type changes (memory vs. backing file). Short reads are
/// expected behavior. On the other hand, [`Self::write()`] will never do a
/// short write. It either writes all of the requested data or fails if reading
/// the original blocks for CoW fails.
pub struct MemoryCowFile<R: Read + Seek> {
    reader: R,
    block_size: u32,
    blocks: HashMap<u64, Vec<u8>>,
    orig_size: u64,
    cur_size: u64,
    cur_offset: u64,
    need_seek: bool,
}

impl<R: Read + Seek> MemoryCowFile<R> {
    pub fn new(mut reader: R, block_size: u32) -> io::Result<Self> {
        assert!(block_size != 0, "Block size cannot be zero");

        let size = reader.seek(SeekFrom::End(0))?;
        reader.rewind()?;

        Ok(Self {
            reader,
            block_size,
            blocks: HashMap::new(),
            orig_size: size,
            cur_size: size,
            cur_offset: 0,
            need_seek: false,
        })
    }

    #[inline]
    fn is_cow_block(&self, block: u64) -> bool {
        // Anything past the original EOF is always a CoW block, even if it's
        // missing from the map (meaning it's a hole).
        self.blocks.contains_key(&block) || block * u64::from(self.block_size) >= self.orig_size
    }
}

impl<R: Read + Seek> Read for MemoryCowFile<R> {
    fn read(&mut self, mut buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.cur_offset >= self.cur_size {
            return Ok(0);
        }

        let block_size = u64::from(self.block_size);
        let start_block = self.cur_offset / block_size;
        let buf_remain = (self.cur_size - self.cur_offset).min(buf.len() as u64);
        let end_block = (self.cur_offset + buf_remain).div_ceil(block_size);
        let first_is_cow = self.is_cow_block(start_block);

        // To ensure that we can't return an error when data has already been
        // partially read, we don't cross block type boundaries.
        let end_block = (start_block + 1..end_block)
            .find(|b| self.is_cow_block(*b) != first_is_cow)
            .unwrap_or(end_block);

        // Ensure buf is not larger than the remaining data for simplicity.
        let end_offset = end_block
            .checked_mul(block_size)
            .unwrap_or(self.cur_size)
            .min(self.cur_size);
        let buf_remain = (end_offset - self.cur_offset).min(buf.len() as u64);
        buf = &mut buf[..buf_remain as usize];

        if first_is_cow {
            for block in start_block..end_block {
                let block_offset = self.cur_offset % block_size;
                let block_remain = block_size - block_offset;
                let to_fill = buf.len().min(block_remain as usize);

                if let Some(data) = self.blocks.get(&block) {
                    buf[..to_fill].copy_from_slice(&data[block_offset as usize..][..to_fill]);
                } else {
                    // This is a hole after the original data.
                    buf[..to_fill].fill(0);
                }

                self.cur_offset += to_fill as u64;
                buf = &mut buf[to_fill..];
            }

            debug_assert!(buf.is_empty(), "Space remaining in buf");

            self.need_seek = true;

            Ok(buf_remain as usize)
        } else {
            // We can do one sequential read until the end of the original data.
            if self.need_seek {
                self.reader.seek(SeekFrom::Start(self.cur_offset))?;
                self.need_seek = false;
            }

            let to_read = (self.orig_size - self.cur_offset).min(buf.len() as u64) as usize;
            debug_assert!(to_read != 0, "Non-CoW block had nothing to read");

            let n = self.reader.read(&mut buf[..to_read])?;

            self.cur_offset += n as u64;

            if self.cur_offset == self.orig_size && to_read > n {
                // The last block might be a partial hole.
                buf[to_read..][..to_read - n].fill(0);

                self.cur_offset += (to_read - n) as u64;

                Ok(to_read)
            } else {
                // This might've been a short read.
                Ok(n)
            }
        }
    }
}

impl<R: Read + Seek> Write for MemoryCowFile<R> {
    fn write(&mut self, mut buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        self.need_seek = true;

        let block_size = u64::from(self.block_size);
        let start_block = self.cur_offset / block_size;
        let end_offset = self
            .cur_offset
            .checked_add(buf.len() as u64)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Write would put offset out-of-bounds",
                )
            })?;
        let end_block = end_offset.div_ceil(block_size);

        // Read in the required blocks for CoW for the first pass. Blocks that
        // would be completely overwritten by the incoming data are skipped. If
        // any of these reads fail, the data from the caller's point of view is
        // unchanged.
        for block in start_block..end_block {
            if let Entry::Vacant(entry) = self.blocks.entry(block) {
                let block_start_offset = block * block_size;
                let block_end_offset =
                    block_start_offset.checked_add(block_size).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "Block end offset out-of-bounds",
                        )
                    })?;

                if block_start_offset < self.orig_size
                    && (block_start_offset < self.cur_offset || block_end_offset > end_offset)
                {
                    let mut data = vec![0u8; block_size as usize];
                    let to_read =
                        (self.orig_size - block_start_offset).min(data.len() as u64) as usize;

                    self.reader.seek(SeekFrom::Start(block_start_offset))?;
                    self.reader.read_exact(&mut data[..to_read])?;

                    entry.insert(data);
                }
            }
        }

        let buf_size = buf.len();

        // Finally, copy in the user data. Everything that can fail is done.
        for block in start_block..end_block {
            let data = self
                .blocks
                .entry(block)
                .or_insert_with(|| vec![0u8; block_size as usize]);
            let block_offset = self.cur_offset % block_size;
            let to_copy = buf.len().min(data.len() - block_offset as usize);

            data[block_offset as usize..][..to_copy].copy_from_slice(&buf[..to_copy]);

            self.cur_offset += to_copy as u64;
            if self.cur_offset > self.cur_size {
                self.cur_size = self.cur_offset;
            }

            buf = &buf[to_copy..];
        }

        Ok(buf_size)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<R: Read + Seek> Seek for MemoryCowFile<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_offset = match pos {
            SeekFrom::Start(o) => o,
            SeekFrom::End(o) => self
                .cur_size
                .checked_add_signed(o)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Out of bounds"))?,
            SeekFrom::Current(o) => self
                .cur_offset
                .checked_add_signed(o)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Out of bounds"))?,
        };

        if self.cur_offset != new_offset {
            self.cur_offset = new_offset;
            self.need_seek = true;
        }

        Ok(self.cur_offset)
    }
}
