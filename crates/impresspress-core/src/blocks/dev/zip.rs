//! A minimal, reproducible ZIP writer — stored entries only.
//!
//! The sandbox export bundle (design's Plan 4) is a zip a static host serves
//! verbatim: `seed/site/*`, `seed/blocks/*.wasm`, `seed/data.json`. Nothing in
//! that set benefits from DEFLATE — wasm modules and already-minified assets
//! barely shrink, and a browser unzipping on demand doesn't need to pay a
//! decompression cost either — so this writer only ever emits method-0
//! (stored) entries, which keeps the whole implementation to the three record
//! types the ZIP format defines (local file header, central directory header,
//! end-of-central-directory) with no compressor pulled in.
//!
//! Every entry carries the same fixed DOS date/time (2026-01-01 00:00:00):
//! two exports of the same tree must produce byte-identical archives, and the
//! wall-clock the export ran at is not part of that identity.
//!
//! No ZIP64 — entries and the archive as a whole are refused once they would
//! carry the classic format's `u32` offset/size fields past 4 GiB. The
//! sandbox's own per-file and per-workspace quotas ([`super::paths`]) keep a
//! real export orders of magnitude under that; this is a hard backstop, not a
//! limit anyone is expected to hit.

use std::collections::HashSet;

/// DOS time field for 00:00:00 — the fixed timestamp every entry carries.
const DOS_TIME: u16 = 0;

/// DOS date field for 2026-01-01: `((2026 - 1980) << 9) | (1 << 5) | 1`.
const DOS_DATE: u16 = 0x5C21;

/// "Version [made by / needed to extract]" — 2.0, the floor for the
/// UTF-8-filename flag this writer always sets.
const VERSION: u16 = 20;

/// General-purpose bit flag: bit 11 (`0x0800`) marks the file name as UTF-8,
/// so a reader trusts `path` verbatim instead of guessing an OEM code page.
const FLAG_UTF8: u16 = 0x0800;

/// Compression method 0 — stored, no compression. See the module docs for
/// why this writer never emits anything else.
const METHOD_STORED: u16 = 0;

const LOCAL_HEADER_SIG: u32 = 0x0403_4b50;
const CENTRAL_HEADER_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;

// The three sizes below are `u64` rather than `usize` because their only use
// is the archive-size arithmetic in `add`, which must not be done in a 32-bit
// type — see `MAX_ARCHIVE_BYTES`.

/// Fixed size of a local file header, before the file name.
const LOCAL_HEADER_FIXED_LEN: u64 = 30;

/// Fixed size of a central directory record, before the file name.
const CENTRAL_HEADER_FIXED_LEN: u64 = 46;

/// Size of the end-of-central-directory record (no archive comment).
const EOCD_LEN: u64 = 22;

/// Ceiling every offset/size field in the classic (non-ZIP64) format can
/// hold.
///
/// `u64`, and the projected total in [`ZipWriter::add`] is widened to match,
/// because `usize` is 32 bits on wasm32 — one of the two targets
/// [`super::export`] serves `/b/dev/api/export` from, the browser sandbox
/// being the other half of the native dev server. As a `usize` this constant
/// WAS `usize::MAX` there, so `projected_total > MAX_ARCHIVE_BYTES` could
/// never be true and the ceiling silently refused nothing on that target. It
/// held only on 64-bit hosts, which is where the tests below run — so no test
/// covers the regression, and none can without allocating 4 GiB.
const MAX_ARCHIVE_BYTES: u64 = u32::MAX as u64;

/// Ceiling on entry count: both the central directory's own header and the
/// end-of-central-directory record count entries in a `u16` field.
const MAX_ENTRIES: usize = u16::MAX as usize;

/// Failure adding one entry to a [`ZipWriter`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZipError {
    /// `path` is absolute, uses `\`, has an empty/`.`/`..` segment, or is
    /// otherwise not the archive-relative, forward-slash form every entry
    /// must use.
    #[error(
        "{0:?} is not a valid zip entry path — it must be relative and forward-slash-separated"
    )]
    BadPath(String),
    /// `path` was already added to this archive.
    #[error("{0:?} was already added to this archive")]
    Duplicate(String),
    /// `path`'s UTF-8 byte length does not fit the format's 16-bit name-length
    /// field (max 65535).
    #[error("{0:?} is longer than the 65535-byte zip entry name limit")]
    PathTooLong(String),
    /// Adding this entry would carry the archive past the 4 GiB ceiling the
    /// classic (non-ZIP64) format's `u32` offset/size fields impose — either
    /// the entry data itself, or the central directory record `finish` will
    /// eventually have to write for it.
    #[error("archive would exceed the 4 GiB limit this stored-entry writer supports (no ZIP64)")]
    TooLarge,
    /// This archive already holds [`MAX_ENTRIES`] entries — one more would
    /// overflow the central directory's 16-bit entry-count fields, wrapping
    /// silently into a corrupt (undercounted) archive instead of refusing.
    #[error("archive already holds the maximum {MAX_ENTRIES} entries this format's 16-bit count fields support")]
    TooManyEntries,
}

/// What [`ZipWriter::finish`] needs to remember about one entry to write its
/// central directory record. The local header's own copy of the same facts
/// is written immediately by [`ZipWriter::add`] and not re-derived from this
/// — this is purely the second record [`finish`](ZipWriter::finish) owes.
struct CentralEntry {
    name: String,
    crc32: u32,
    size: u32,
    offset: u32,
}

/// Builds a ZIP archive of stored (uncompressed) entries, byte-for-byte
/// reproducible across runs for the same inputs in the same order. See the
/// module docs for the format and reproducibility rationale.
pub struct ZipWriter {
    buf: Vec<u8>,
    entries: Vec<CentralEntry>,
    /// Mirrors `entries`' names as a set so [`ZipWriter::add`]'s duplicate
    /// check is O(1) rather than an O(n) scan of `entries` per call — an
    /// export bundle can carry thousands of source files.
    names: HashSet<String>,
    /// Running total of every added entry's own central directory record
    /// size (`CENTRAL_HEADER_FIXED_LEN + name.len()`). Kept incrementally so
    /// `add` can refuse an entry that would carry `finish`'s eventual output
    /// — data already written, plus the central directory, plus the EOCD —
    /// past the 4 GiB ceiling, without `finish` itself needing to be
    /// fallible.
    ///
    /// `u64` to match the arithmetic it feeds, not because it can overflow:
    /// the ceiling check in [`ZipWriter::add`] reads this field and refuses
    /// the entry BEFORE the increment that would grow it, so it stays under
    /// `MAX_ARCHIVE_BYTES` and would fit a 32-bit `usize`. The type keeps the
    /// projected-size expression in one width instead of casting this operand
    /// where that expression reads it.
    central_dir_bytes: u64,
}

impl Default for ZipWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl ZipWriter {
    /// An archive with no entries.
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            entries: Vec::new(),
            names: HashSet::new(),
            central_dir_bytes: 0,
        }
    }

    /// Add one stored entry. `path` must be relative (no leading `/`, no `.`
    /// or `..` segment, no empty segment — i.e.
    /// [`wafer_block::wrap::is_traversal_safe_path`]), forward-slash-separated
    /// (no `\`), unique within this archive, and at most 65535 UTF-8 bytes
    /// long.
    ///
    /// Every check runs, and every fallible size cast happens, before
    /// anything is written or recorded — a rejected `add` leaves the writer
    /// exactly as it was, so a caller that wraps this in its own retry logic
    /// never has to reason about a half-applied entry.
    pub fn add(&mut self, path: &str, bytes: &[u8]) -> Result<(), ZipError> {
        if path.contains('\\') || !wafer_block::wrap::is_traversal_safe_path(path) {
            return Err(ZipError::BadPath(path.to_string()));
        }
        if path.len() > u16::MAX as usize {
            return Err(ZipError::PathTooLong(path.to_string()));
        }
        if self.names.contains(path) {
            return Err(ZipError::Duplicate(path.to_string()));
        }
        // Checked before the size math below: a 65536th entry would wrap
        // silently when `finish` casts `entries.len()` to `u16`, producing a
        // corrupt archive that *looks* like it has fewer entries than it
        // does rather than failing loudly.
        if self.entries.len() >= MAX_ENTRIES {
            return Err(ZipError::TooManyEntries);
        }

        let offset = u32::try_from(self.buf.len()).map_err(|_| ZipError::TooLarge)?;
        let size = u32::try_from(bytes.len()).map_err(|_| ZipError::TooLarge)?;
        // `u64` throughout, not `usize`: `offset` and `size` above cap
        // `buf.len()` and `bytes.len()` at `u32::MAX` each, so on wasm32 —
        // where `usize` is 32 bits — their sum overflows the type this used to
        // be computed in, before the ceiling below could refuse it. `path_len`
        // is `u16::MAX`-bounded by the check above, so the cast is lossless.
        let path_len = path.len() as u64;
        let grows_by = LOCAL_HEADER_FIXED_LEN + path_len + u64::from(size);
        let central_entry_len = CENTRAL_HEADER_FIXED_LEN + path_len;
        // The full projected size of `finish`'s eventual output: this
        // entry's local header + data, every central directory record
        // (already-written ones plus this one), and the EOCD — not just the
        // data written so far. `finish` itself cannot fail, so every byte it
        // will ever write has to be accounted for here.
        let projected_total = u64::from(offset)
            .saturating_add(grows_by)
            .saturating_add(self.central_dir_bytes)
            .saturating_add(central_entry_len)
            .saturating_add(EOCD_LEN);
        if projected_total > MAX_ARCHIVE_BYTES {
            return Err(ZipError::TooLarge);
        }

        let crc32 = crc32fast::hash(bytes);
        write_local_header(&mut self.buf, path, crc32, size);
        self.buf.extend_from_slice(bytes);

        self.names.insert(path.to_string());
        self.central_dir_bytes += central_entry_len;
        self.entries.push(CentralEntry {
            name: path.to_string(),
            crc32,
            size,
            offset,
        });
        Ok(())
    }

    /// Consume the writer and return the complete archive: every entry's
    /// local header and data (already written by `add`), then the central
    /// directory, then the end-of-central-directory record.
    pub fn finish(self) -> Vec<u8> {
        let mut buf = self.buf;
        let central_start = buf.len();
        for entry in &self.entries {
            write_central_header(&mut buf, entry);
        }
        let central_size = buf.len() - central_start;
        write_eocd(&mut buf, self.entries.len(), central_size, central_start);
        buf
    }
}

/// Local file header (`PK\x03\x04`) plus the entry's raw bytes — everything
/// [`ZipWriter::add`] writes immediately, ahead of the central directory.
fn write_local_header(buf: &mut Vec<u8>, path: &str, crc32: u32, size: u32) {
    buf.extend_from_slice(&LOCAL_HEADER_SIG.to_le_bytes());
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.extend_from_slice(&FLAG_UTF8.to_le_bytes());
    buf.extend_from_slice(&METHOD_STORED.to_le_bytes());
    buf.extend_from_slice(&DOS_TIME.to_le_bytes());
    buf.extend_from_slice(&DOS_DATE.to_le_bytes());
    buf.extend_from_slice(&crc32.to_le_bytes());
    // Stored entries have no compression, so compressed size == uncompressed.
    buf.extend_from_slice(&size.to_le_bytes());
    buf.extend_from_slice(&size.to_le_bytes());
    buf.extend_from_slice(&(path.len() as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // extra field length
    buf.extend_from_slice(path.as_bytes());
}

/// One central directory record (`PK\x01\x02`) — the same facts the local
/// header carries, plus the offset a reader needs to seek to it.
fn write_central_header(buf: &mut Vec<u8>, entry: &CentralEntry) {
    buf.extend_from_slice(&CENTRAL_HEADER_SIG.to_le_bytes());
    buf.extend_from_slice(&VERSION.to_le_bytes()); // version made by
    buf.extend_from_slice(&VERSION.to_le_bytes()); // version needed to extract
    buf.extend_from_slice(&FLAG_UTF8.to_le_bytes());
    buf.extend_from_slice(&METHOD_STORED.to_le_bytes());
    buf.extend_from_slice(&DOS_TIME.to_le_bytes());
    buf.extend_from_slice(&DOS_DATE.to_le_bytes());
    buf.extend_from_slice(&entry.crc32.to_le_bytes());
    buf.extend_from_slice(&entry.size.to_le_bytes());
    buf.extend_from_slice(&entry.size.to_le_bytes());
    buf.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // extra field length
    buf.extend_from_slice(&0u16.to_le_bytes()); // file comment length
    buf.extend_from_slice(&0u16.to_le_bytes()); // disk number start
    buf.extend_from_slice(&0u16.to_le_bytes()); // internal file attributes
    buf.extend_from_slice(&0u32.to_le_bytes()); // external file attributes
    buf.extend_from_slice(&entry.offset.to_le_bytes());
    buf.extend_from_slice(entry.name.as_bytes());
}

/// The end-of-central-directory record (`PK\x05\x06`) — the last thing a zip
/// reader looks for, since it is what points back at everything else.
fn write_eocd(buf: &mut Vec<u8>, entry_count: usize, central_size: usize, central_offset: usize) {
    buf.extend_from_slice(&EOCD_SIG.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // number of this disk
    buf.extend_from_slice(&0u16.to_le_bytes()); // disk where central directory starts
    buf.extend_from_slice(&(entry_count as u16).to_le_bytes());
    buf.extend_from_slice(&(entry_count as u16).to_le_bytes());
    buf.extend_from_slice(&(central_size as u32).to_le_bytes());
    buf.extend_from_slice(&(central_offset as u32).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // comment length
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_reads_back_with_the_zip_crate() {
        let mut w = ZipWriter::new();
        w.add("README.md", b"hello").unwrap();
        w.add("seed/site/index.html", b"<h1>x</h1>").unwrap();
        w.add("seed/blocks/hello.wasm", &[0, 97, 115, 109, 1, 0, 0, 0])
            .unwrap();
        let bytes = w.finish();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 3);
        let mut f = archive.by_name("seed/site/index.html").unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut f, &mut s).unwrap();
        assert_eq!(s, "<h1>x</h1>");
        assert_eq!(f.compression(), zip::CompressionMethod::Stored);
        assert_eq!(f.crc32(), crc32fast::hash(b"<h1>x</h1>"));
    }

    #[test]
    fn duplicate_and_absolute_paths_are_rejected() {
        let mut w = ZipWriter::new();
        w.add("a", b"1").unwrap();
        assert!(matches!(w.add("a", b"2"), Err(ZipError::Duplicate(_))));
        assert!(matches!(w.add("/a", b"2"), Err(ZipError::BadPath(_))));
        assert!(matches!(w.add("a\\b", b"2"), Err(ZipError::BadPath(_))));
    }

    #[test]
    fn traversal_and_empty_segments_are_rejected() {
        let mut w = ZipWriter::new();
        assert!(matches!(w.add("", b"1"), Err(ZipError::BadPath(_))));
        assert!(matches!(w.add("a/../b", b"1"), Err(ZipError::BadPath(_))));
        assert!(matches!(w.add("./a", b"1"), Err(ZipError::BadPath(_))));
        assert!(matches!(w.add("a//b", b"1"), Err(ZipError::BadPath(_))));
        assert!(matches!(w.add("a/", b"1"), Err(ZipError::BadPath(_))));
    }

    /// 65535 tiny entries is fine; a 65536th is refused rather than wrapping
    /// the central directory's `u16` entry-count field into a silently
    /// undercounted (corrupt) archive. Real entries, not a shrunk limit —
    /// 65535 one-byte-named, zero-byte entries is well under a second.
    #[test]
    fn refuses_more_than_max_entries() {
        let mut w = ZipWriter::new();
        for i in 0..MAX_ENTRIES {
            w.add(&format!("f{i}"), b"")
                .unwrap_or_else(|e| panic!("entry {i}: {e}"));
        }
        assert!(matches!(
            w.add("one-too-many", b""),
            Err(ZipError::TooManyEntries)
        ));
    }
}
