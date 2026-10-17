//! Bounded-memory last-record index for an append-only journal.
//!
//! Only keys and byte offsets are sorted. Larger histories spill to private
//! temporary runs outside the workspace; at most FAN_IN runs are merged at
//! once. Completed job history therefore does not become a resident hash map.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};

use tempfile::{NamedTempFile, TempPath};

const CHUNK_BYTES: usize = 4 * 1024 * 1024;
const FAN_IN: usize = 16;
const MAX_KEY_BYTES: usize = super::MAX_ROW_BYTES;

type Entry = (String, u64);

#[derive(Default)]
pub(super) struct LatestIndex {
    entries: Vec<Entry>,
    bytes: usize,
    levels: Vec<Vec<TempPath>>,
}

impl LatestIndex {
    pub(super) fn insert(&mut self, key: String, offset: u64) -> io::Result<()> {
        if key.len() > MAX_KEY_BYTES {
            return Err(invalid("daemon job key exceeds the record limit"));
        }
        self.bytes += key.capacity() + std::mem::size_of::<Entry>();
        self.entries.push((key, offset));
        if self.bytes >= CHUNK_BYTES {
            self.spill()?;
        }
        Ok(())
    }

    fn spill(&mut self) -> io::Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        collapse(&mut self.entries);
        let mut file = NamedTempFile::new()?;
        {
            let mut writer = BufWriter::new(file.as_file_mut());
            for entry in &self.entries {
                write_entry(&mut writer, entry)?;
            }
            writer.flush()?;
        }
        self.entries.clear();
        self.bytes = 0;
        let mut run = file.into_temp_path();
        let mut level = 0;
        loop {
            if level == self.levels.len() {
                self.levels.push(Vec::new());
            }
            self.levels[level].push(run);
            if self.levels[level].len() < FAN_IN {
                break;
            }
            run = merge(std::mem::take(&mut self.levels[level]))?;
            level += 1;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> io::Result<LatestKeys> {
        if self.levels.is_empty() {
            collapse(&mut self.entries);
            return Ok(LatestKeys::Memory(self.entries.into_iter()));
        }
        self.spill()?;
        // Each level has fewer than FAN_IN runs. A u64 byte offset bounds the
        // number of levels; paths and descriptors cannot grow with every row.
        let mut runs: Vec<_> = self.levels.into_iter().flatten().collect();
        while runs.len() > 1 {
            let mut next = Vec::new();
            let mut source = runs.into_iter();
            loop {
                let batch: Vec<_> = source.by_ref().take(FAN_IN).collect();
                if batch.is_empty() {
                    break;
                }
                next.push(merge(batch)?);
            }
            runs = next;
        }
        let path = runs.pop().ok_or_else(|| invalid("empty daemon index merge"))?;
        let reader = BufReader::new(File::open(&path)?);
        Ok(LatestKeys::Disk { reader, _path: path })
    }
}

fn collapse(entries: &mut Vec<Entry>) {
    entries.sort_unstable();
    entries.dedup_by(|later, earlier| {
        if later.0 == earlier.0 {
            earlier.1 = earlier.1.max(later.1);
            true
        } else {
            false
        }
    });
}

pub(super) enum LatestKeys {
    Memory(std::vec::IntoIter<Entry>),
    Disk { reader: BufReader<File>, _path: TempPath },
}

impl Iterator for LatestKeys {
    type Item = io::Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Memory(entries) => entries.next().map(Ok),
            Self::Disk { reader, .. } => read_entry(reader).transpose(),
        }
    }
}

fn merge(runs: Vec<TempPath>) -> io::Result<TempPath> {
    if runs.len() == 1 {
        return runs.into_iter().next().ok_or_else(|| invalid("empty merge"));
    }
    let mut readers = runs
        .iter()
        .map(|path| File::open(path).map(BufReader::new))
        .collect::<io::Result<Vec<_>>>()?;
    let mut heap = BinaryHeap::new();
    for (source, reader) in readers.iter_mut().enumerate() {
        if let Some((key, offset)) = read_entry(reader)? {
            heap.push(Reverse((key, offset, source)));
        }
    }
    let mut output = NamedTempFile::new()?;
    {
        let mut writer = BufWriter::new(output.as_file_mut());
        let mut pending: Option<Entry> = None;
        while let Some(Reverse((key, offset, source))) = heap.pop() {
            match pending.as_mut() {
                Some((old_key, old_offset)) if *old_key == key => {
                    *old_offset = (*old_offset).max(offset);
                }
                _ => {
                    if let Some(entry) = pending.take() {
                        write_entry(&mut writer, &entry)?;
                    }
                    pending = Some((key, offset));
                }
            }
            if let Some((key, offset)) = read_entry(&mut readers[source])? {
                heap.push(Reverse((key, offset, source)));
            }
        }
        if let Some(entry) = pending {
            write_entry(&mut writer, &entry)?;
        }
        writer.flush()?;
    }
    // Readers close before their TempPaths are dropped (required on Windows).
    drop(readers);
    drop(runs);
    Ok(output.into_temp_path())
}

fn write_entry(writer: &mut impl Write, entry: &Entry) -> io::Result<()> {
    let length = u32::try_from(entry.0.len()).map_err(|_| invalid("oversized index key"))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(entry.0.as_bytes())?;
    writer.write_all(&entry.1.to_le_bytes())
}

fn read_entry(reader: &mut impl Read) -> io::Result<Option<Entry>> {
    let mut length = [0_u8; 4];
    if reader.read(&mut length[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut length[1..])?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_KEY_BYTES {
        return Err(invalid("oversized daemon index key"));
    }
    let mut key = vec![0; length];
    reader.read_exact(&mut key)?;
    let key = String::from_utf8(key).map_err(|_| invalid("invalid daemon index key"))?;
    let mut offset = [0_u8; 8];
    reader.read_exact(&mut offset)?;
    Ok(Some((key, u64::from_le_bytes(offset))))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn memory_index_uses_last_offset_not_insertion_order() -> io::Result<()> {
        let mut index = LatestIndex::default();
        for (key, offset) in [("b", 20), ("a", 30), ("b", 10), ("a", 5)] {
            index.insert(key.to_owned(), offset)?;
        }
        let entries = index.finish()?.collect::<io::Result<Vec<_>>>()?;
        assert_eq!(entries, vec![("a".to_owned(), 30), ("b".to_owned(), 20)]);
        Ok(())
    }

    #[test]
    fn spills_and_multiple_merge_levels_match_an_unbounded_oracle() -> io::Result<()> {
        let mut index = LatestIndex::default();
        let mut expected = BTreeMap::new();
        for offset in 0..2_000_u64 {
            let key = format!("job-{:05}", (offset * 31) % 337);
            expected.insert(key.clone(), offset);
            index.insert(key, offset)?;
            // Force more than FAN_IN runs without a huge test allocation.
            if offset % 13 == 0 {
                index.spill()?;
            }
        }
        let actual = index.finish()?.collect::<io::Result<BTreeMap<_, _>>>()?;
        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn automatic_spill_bounds_the_in_memory_key_chunk() -> io::Result<()> {
        let mut index = LatestIndex::default();
        for offset in 0..100_000_u64 {
            index.insert(format!("run-00000000-0000-0000-0000-{offset:012}:tick-000001:job-000001"), offset)?;
            assert!(index.bytes < CHUNK_BYTES);
        }
        assert!(!index.levels.is_empty());
        assert!(index.levels.iter().all(|runs| runs.len() < FAN_IN));
        assert_eq!(index.finish()?.collect::<io::Result<Vec<_>>>()?.len(), 100_000);
        Ok(())
    }

    #[test]
    fn truncated_spill_record_is_an_error_not_end_of_history() {
        for bytes in [vec![1], vec![1, 0, 0, 0, b'x']] {
            assert!(read_entry(&mut bytes.as_slice()).is_err());
        }
    }
}
