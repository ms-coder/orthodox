//! Resource guards against decompression bombs and hostile archives (ADR-0008).

/// Limits applied while indexing and streaming. `Default` is what the app uses; tests shrink them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// Hard cap on the number of entries; indexing fails with `LimitExceeded(EntryCount)`.
    /// Index memory is roughly 200 bytes + path length per entry.
    pub max_entries: usize,
    /// Hard cap on path depth (components); deeper entries fail indexing with `LimitExceeded(Depth)`.
    pub max_depth: usize,
    /// Declared total unpacked size above which `IndexWarnings::total_size_exceeds_warn` is set
    /// (the UI/extract job asks the user to confirm; it is never a hard failure).
    pub warn_total_bytes: u64,
    /// An entry whose declared size is at least `ratio_min_size` and exceeds `max_ratio` times its
    /// packed size counts in `IndexWarnings::suspicious_ratio_entries` (zip bombs: 1000:1 and up).
    pub max_ratio: u64,
    /// Minimum declared size for the ratio guard to apply (tiny files compress absurdly well).
    pub ratio_min_size: u64,
    /// Memory a single codec may allocate (LZMA/LZMA2 dictionary, xz block, zstd window). Over the
    /// limit = `LimitExceeded(CodecMemory)`, never an allocation.
    pub max_codec_memory: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        ArchiveLimits {
            max_entries: 500_000,
            max_depth: 256,
            warn_total_bytes: 64 << 30,
            max_ratio: 1000,
            ratio_min_size: 1 << 20,
            max_codec_memory: 256 << 20,
        }
    }
}
