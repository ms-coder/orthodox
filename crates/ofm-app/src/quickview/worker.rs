//! Preview production: everything here runs on worker threads (never the GTK main thread) and
//! produces plain `Send` data ([`Preview`]); the main thread only turns it into widgets.
//!
//! - text: [`ofm_viewer`] reads the first lines (mmap, so a 10 GB file is as fast as a small one),
//! - images: gdk-pixbuf decodes at the target size (JPEG decodes with DCT scaling) and the raw
//!   pixels travel to the main thread as `glib::Bytes` (a texture is created there),
//! - directories: the jobs size walk ([`ofm_jobs::ops::size`]) on a dedicated thread, cancellable,
//!   reporting running totals,
//! - everything else (binary, special files, broken links): an information table.
//!
//! Every job takes a [`CancelToken`]: when the cursor moves on, the controller cancels it and
//! drops whatever the worker still sends (results carry the request generation).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::gdk_pixbuf::Pixbuf;
use gtk::gio;
use gtk::glib;
use ofm_core::{CancelToken, EntryKind, TargetKind};
use ofm_jobs::ops::size;
use ofm_jobs::{Fs, JobCtx, JobSink, PauseGate, Progress, RealFs, SystemClock, event::SizeTotals};
use ofm_viewer::{
    Document, Encoding, Preview as ViewerPreview, PreviewProvider, TextPreviewProvider,
};
use ofm_viewer::{decode_range, detect_encoding};

use crate::chrome::statusline::{StatusFormat, format_size, format_time};
use crate::i18n::tr;

/// Lines of a text preview are cut here (a minified one-liner must not become a huge layout).
const MAX_LINE_CHARS: usize = 400;
/// Bytes sniffed to tell text from binary.
const SNIFF_LEN: usize = 8192;
/// Larger image files are not decoded (information table instead).
const MAX_IMAGE_FILE: u64 = 256 * 1024 * 1024;
/// Images with more pixels than this are not decoded (a tiny file can declare a huge canvas).
const MAX_IMAGE_PIXELS: u64 = 100_000_000;
/// Images wider or taller than this are not decoded.
const MAX_IMAGE_SIDE: i32 = 30_000;
/// Running totals of a directory walk are reported at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(120);

/// File extensions treated as images (decoded by gdk-pixbuf; whether it really can is decided
/// by the loader, failure falls back to the information table).
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "gif", "bmp", "webp", "tif", "tiff", "ico", "svg", "xpm", "tga",
    "pnm", "ppm", "pgm", "pbm", "qoi", "avif", "heic",
];

/// The item to preview: plain data copied from the panel entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// Real local path.
    pub path: PathBuf,
    /// Display name (file name).
    pub name: OsString,
    /// Kind from the listing.
    pub kind: EntryKind,
    /// Size in bytes from the listing.
    pub size: u64,
    /// Modification time (ns since the epoch) from the listing.
    pub mtime_ns: i128,
    /// `st_mode` from the listing.
    pub mode: u32,
}

/// What identifies "the same preview": a changed size or mtime (the file was edited) restarts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetKey {
    path: PathBuf,
    kind: EntryKind,
    size: u64,
    mtime_ns: i128,
}

impl Target {
    /// Identity for "is this already shown / pending".
    pub fn key(&self) -> TargetKey {
        TargetKey {
            path: self.path.clone(),
            kind: self.kind,
            size: self.size,
            mtime_ns: self.mtime_ns,
        }
    }

    /// Directory or symlink to a directory.
    pub fn is_dir(&self) -> bool {
        self.kind.is_dir_like()
    }
}

/// How much the pane can show (pixels and text rows), so workers do not produce more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewBox {
    /// Width in device pixels.
    pub width: i32,
    /// Height in device pixels.
    pub height: i32,
    /// Text rows that fit.
    pub rows: usize,
}

/// A label/value line of an information table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Left column.
    pub label: String,
    /// Right column.
    pub value: String,
}

impl Row {
    fn new(label: &str, value: impl Into<String>) -> Row {
        Row {
            label: label.to_owned(),
            value: value.into(),
        }
    }
}

/// Decoded image pixels (not a texture yet: creating it is the main thread's job).
pub struct ImageData {
    /// Pixel width.
    pub width: i32,
    /// Pixel height.
    pub height: i32,
    /// Bytes per row.
    pub stride: usize,
    /// 4 channels (RGBA, not premultiplied) instead of 3 (RGB).
    pub alpha: bool,
    /// The pixels.
    pub bytes: glib::Bytes,
    /// "1920 x 1080 - PNG - 2.1 MiB".
    pub caption: String,
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageData")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// A finished preview.
#[derive(Debug)]
pub enum Preview {
    /// The first lines of a text file.
    Text {
        /// Display-safe lines (tabs expanded, control characters replaced, long lines cut).
        lines: Vec<String>,
        /// "UTF-8", "windows-1251", ...
        encoding: &'static str,
        /// The whole file is shown.
        complete: bool,
    },
    /// A decoded image.
    Image(ImageData),
    /// Name/size/time/type table (binary and special files, unreadable files).
    Info {
        /// Table rows.
        rows: Vec<Row>,
        /// Optional dim note under the table.
        note: Option<String>,
    },
    /// A one-line message (empty file, parent directory, ...).
    Message(String),
}

/// What a worker sends to the main thread.
#[derive(Debug)]
pub enum Update {
    /// The preview is ready.
    Ready(Preview),
    /// Directory walk: running totals.
    DirProgress {
        /// Files seen so far.
        files: u64,
        /// Their bytes.
        bytes: u64,
    },
    /// Directory walk finished.
    DirDone(SizeTotals),
    /// The walk failed (message).
    DirFailed(String),
}

/// An [`Update`] tagged with the request it belongs to (stale generations are dropped).
#[derive(Debug)]
pub struct Msg {
    /// Request generation.
    pub generation: u64,
    /// The payload.
    pub update: Update,
}

/// Whether `name` has an image extension.
pub fn is_image_name(name: &std::ffi::OsStr) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|i| i.eq_ignore_ascii_case(e)))
}

/// `-rwxr-xr-x` style permission string.
pub fn mode_string(mode: u32) -> String {
    let t = match mode & 0o170_000 {
        0o040_000 => 'd',
        0o120_000 => 'l',
        0o060_000 => 'b',
        0o020_000 => 'c',
        0o010_000 => 'p',
        0o140_000 => 's',
        _ => '-',
    };
    let mut s = String::from(t);
    for (shift, special) in [(6, 0o4000), (3, 0o2000), (0, 0o1000)] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        let exec = bits & 1 != 0;
        s.push(match (mode & special != 0, exec) {
            (true, true) => {
                if shift == 0 {
                    't'
                } else {
                    's'
                }
            }
            (true, false) => {
                if shift == 0 {
                    'T'
                } else {
                    'S'
                }
            }
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// Make one text line safe to draw: tabs to 8-column stops, control characters to `·`, cut at
/// [`MAX_LINE_CHARS`].
pub fn sanitize_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len().min(MAX_LINE_CHARS + 8));
    let mut col = 0usize;
    for c in line.chars() {
        if col >= MAX_LINE_CHARS {
            out.push('\u{2026}');
            break;
        }
        match c {
            '\t' => {
                let n = 8 - col % 8;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            c if c.is_control() => {
                out.push('\u{b7}');
                col += 1;
            }
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

/// Heuristic: NUL bytes (outside UTF-16 text) or many control characters mean binary data.
/// UTF-16 is trusted when a BOM says so; a BOM-less guess must also decode to mostly
/// printable text (random binary data easily looks like "ASCII in UTF-16").
pub fn looks_binary(head: &[u8], encoding: Encoding, bom_len: usize) -> bool {
    if head.is_empty() {
        return false;
    }
    if matches!(encoding, Encoding::Utf16Le | Encoding::Utf16Be) {
        if bom_len > 0 {
            return false;
        }
        let text = decode_range(encoding, head);
        let total = text.chars().count().max(1);
        let controls = text
            .chars()
            .filter(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r' | '\u{c}'))
            .count();
        return controls * 10 > total;
    }
    if head.contains(&0) {
        return true;
    }
    let controls = head
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0c | 0x1b | 0x08))
        .count();
    controls * 10 > head.len()
}

/// `12.3 KiB (12 589 B)`; just `512 B` below 1 KiB, where both forms are the same number.
pub fn size_exact(bytes: u64, fmt: crate::chrome::statusline::SizeFormat) -> String {
    let human = format_size(bytes, fmt);
    let exact = format_size(bytes, crate::chrome::statusline::SizeFormat::Bytes);
    if bytes < 1000 || human == exact {
        human
    } else {
        format!("{human} ({exact} B)")
    }
}

/// The common rows of an information table (type, size, modified, permissions).
pub fn info_rows(t: &Target, type_text: String, fmt: &StatusFormat) -> Vec<Row> {
    let mut rows = vec![Row::new(tr("Type"), type_text)];
    if !t.is_dir() {
        rows.push(Row::new(tr("Size"), size_exact(t.size, fmt.size)));
    }
    let when = format_time(t.mtime_ns, &fmt.time);
    if !when.is_empty() {
        rows.push(Row::new(tr("Modified"), when));
    }
    if t.mode != 0 {
        rows.push(Row::new(
            tr("Permissions"),
            format!("{} ({:04o})", mode_string(t.mode), t.mode & 0o7777),
        ));
    }
    rows
}

/// Human description of the file's type (shared-mime-info), from the name and the first bytes.
fn type_description(path: &Path, head: &[u8]) -> String {
    let (ctype, _) = gio::content_type_guess(Some(path), head);
    let desc = gio::content_type_get_description(&ctype);
    if desc.is_empty() {
        ctype.to_string()
    } else {
        format!("{desc} ({ctype})")
    }
}

fn read_head(path: &Path) -> Vec<u8> {
    Document::open(path)
        .ok()
        .and_then(|d| d.read_range(0, SNIFF_LEN).ok())
        .unwrap_or_default()
}

fn text_preview(t: &Target, view: ViewBox, cancel: &CancelToken) -> Option<Preview> {
    let provider = TextPreviewProvider {
        max_lines: view.rows.clamp(20, 200),
        max_bytes: 256 * 1024,
    };
    if !provider.can_preview(&t.path) {
        return None;
    }
    match provider.preview(&t.path, cancel).ok()? {
        ViewerPreview::Text {
            lines,
            encoding,
            total_lines,
        } => Some(Preview::Text {
            lines: lines.iter().map(|l| sanitize_line(l)).collect(),
            encoding: encoding.name(),
            complete: total_lines.is_some(),
        }),
        _ => None,
    }
}

/// Whether an image of `w` x `h` pixels may be decoded (header values, before any decoding).
pub fn image_dims_ok(w: i32, h: i32) -> bool {
    w > 0
        && h > 0
        && w <= MAX_IMAGE_SIDE
        && h <= MAX_IMAGE_SIDE
        && u64::from(w.unsigned_abs()) * u64::from(h.unsigned_abs()) <= MAX_IMAGE_PIXELS
}

/// Size of the file as seen through an opened handle (not the listing: for a symlink the
/// listing holds the link's own size). `None` when it cannot be opened or is not a regular file.
fn opened_file_size(path: &Path) -> Option<u64> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NONBLOCK: a FIFO swapped in after the listing must not block this worker on open.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    meta.is_file().then_some(meta.len())
}

fn image_preview(t: &Target, view: ViewBox, fmt: &StatusFormat) -> Option<Preview> {
    let size = opened_file_size(&t.path)?;
    // Header only: nothing is decoded before the declared dimensions pass the cap.
    let (format, w, h) = Pixbuf::file_info(&t.path)?;
    let format_name = format
        .name()
        .map(|n| n.to_ascii_uppercase())
        .unwrap_or_default();
    if size > MAX_IMAGE_FILE || !image_dims_ok(w, h) {
        let real = Target { size, ..t.clone() };
        let mut rows = info_rows(&real, format!("{format_name} {}", tr("image")), fmt);
        rows.push(Row::new(tr("Dimensions"), format!("{w} \u{d7} {h}")));
        return Some(Preview::Info {
            rows,
            note: Some(tr("Image too large to preview").to_owned()),
        });
    }
    let (max_w, max_h) = (view.width.max(64), view.height.max(64));
    let pixbuf = if w <= max_w && h <= max_h {
        Pixbuf::from_file(&t.path).ok()?
    } else {
        Pixbuf::from_file_at_scale(&t.path, max_w, max_h, true).ok()?
    };
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let caption = format!(
        "{w} \u{d7} {h} \u{b7} {} \u{b7} {}",
        format_name,
        format_size(size, fmt.size)
    );
    Some(Preview::Image(ImageData {
        width: pixbuf.width(),
        height: pixbuf.height(),
        stride: usize::try_from(pixbuf.rowstride()).ok()?,
        alpha: pixbuf.has_alpha(),
        bytes: pixbuf.read_pixel_bytes(),
        caption,
    }))
}

/// Produce the preview of a non-directory item. Blocking: run on a worker.
pub fn build(t: &Target, view: ViewBox, fmt: &StatusFormat, cancel: &CancelToken) -> Preview {
    if t.name == ".." {
        return Preview::Message(tr("Parent directory").to_owned());
    }
    match t.kind {
        EntryKind::Symlink { target: None } => {
            return Preview::Info {
                rows: info_rows(t, tr("Broken symbolic link").to_owned(), fmt),
                note: None,
            };
        }
        EntryKind::Other
        | EntryKind::Symlink {
            target: Some(TargetKind::Other),
        } => {
            // Never open these: a FIFO would block the worker forever.
            return Preview::Info {
                rows: info_rows(
                    t,
                    tr("Special file (device, pipe or socket)").to_owned(),
                    fmt,
                ),
                note: None,
            };
        }
        _ => {}
    }
    if is_image_name(&t.name)
        && !cancel.is_cancelled()
        && let Some(p) = image_preview(t, view, fmt)
    {
        return p;
    }
    if cancel.is_cancelled() {
        return Preview::Message(String::new());
    }
    if t.size == 0 {
        return Preview::Message(tr("Empty file").to_owned());
    }
    let head = read_head(&t.path);
    if head.is_empty() {
        return Preview::Info {
            rows: info_rows(t, tr("Unknown").to_owned(), fmt),
            note: Some(tr("The file cannot be read.").to_owned()),
        };
    }
    let detection = detect_encoding(&head);
    if looks_binary(&head, detection.encoding, detection.bom_len) {
        return Preview::Info {
            rows: info_rows(t, type_description(&t.path, &head), fmt),
            note: Some(tr("Binary file").to_owned()),
        };
    }
    text_preview(t, view, cancel).unwrap_or_else(|| Preview::Info {
        rows: info_rows(t, type_description(&t.path, &head), fmt),
        note: None,
    })
}

/// Forwards size-walk progress to the channel, at most every [`PROGRESS_EVERY`].
struct ProgressSink {
    tx: async_channel::Sender<Msg>,
    generation: u64,
    last: Instant,
}

impl JobSink for ProgressSink {
    fn progress(&mut self, p: &Progress) {
        if self.last.elapsed() < PROGRESS_EVERY {
            return;
        }
        self.last = Instant::now();
        let _ = self.tx.try_send(Msg {
            generation: self.generation,
            update: Update::DirProgress {
                files: p.files_total,
                bytes: p.bytes_total,
            },
        });
    }

    fn ask(&mut self, _question: ofm_jobs::event::Question) -> Option<ofm_jobs::event::Answer> {
        None // the size walk never asks
    }
}

/// Walk `path` with the jobs size machinery. Blocking: run on a dedicated thread.
pub fn dir_size(
    path: &Path,
    cancel: &CancelToken,
    generation: u64,
    tx: &async_channel::Sender<Msg>,
) {
    let fs: Arc<dyn Fs> = Arc::new(RealFs);
    let sink = ProgressSink {
        tx: tx.clone(),
        generation,
        last: Instant::now(),
    };
    let mut ctx = JobCtx::new(
        fs,
        cancel.clone(),
        PauseGate::new(),
        Box::new(sink),
        Arc::new(SystemClock::new()),
    );
    let update = match size::run(&mut ctx, &[path.to_path_buf()]) {
        Ok(totals) => Update::DirDone(totals),
        Err(ofm_jobs::JobError::Cancelled) => return,
        Err(e) => Update::DirFailed(e.to_string()),
    };
    let _ = tx.send_blocking(Msg { generation, update });
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test fixtures on the test thread
)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    use ofm_testkit::temp::TempDir;

    use super::*;

    const VIEW: ViewBox = ViewBox {
        width: 400,
        height: 300,
        rows: 30,
    };

    fn target(dir: &Path, name: &str, content: &[u8]) -> Target {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        Target {
            path,
            name: name.into(),
            kind: EntryKind::File,
            size: content.len() as u64,
            mtime_ns: 0,
            mode: 0o100_644,
        }
    }

    fn run(t: &Target) -> Preview {
        build(t, VIEW, &StatusFormat::default(), &CancelToken::new())
    }

    #[test]
    fn text_file_gives_its_first_lines() {
        let d = TempDir::new().unwrap();
        let t = target(d.path(), "a.txt", b"one\ntwo\n\tthree\n");
        match run(&t) {
            Preview::Text {
                lines,
                encoding,
                complete,
            } => {
                assert_eq!(lines, ["one", "two", "        three"]);
                assert_eq!(encoding, "utf-8");
                assert!(complete);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn huge_text_is_cut_to_the_visible_rows() {
        let d = TempDir::new().unwrap();
        let body: String = (0..10_000).map(|i| format!("line {i}\n")).collect();
        let t = target(d.path(), "big.log", body.as_bytes());
        match run(&t) {
            Preview::Text {
                lines, complete, ..
            } => {
                assert_eq!(lines.len(), 30);
                assert_eq!(lines[0], "line 0");
                assert!(!complete);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sparse_ten_gigabyte_file_is_instant() {
        let d = TempDir::new().unwrap();
        let path = d.path().join("sparse.bin");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(10 * 1024 * 1024 * 1024).unwrap();
        drop(f);
        let t = Target {
            path,
            name: "sparse.bin".into(),
            kind: EntryKind::File,
            size: 10 * 1024 * 1024 * 1024,
            mtime_ns: 0,
            mode: 0o100_644,
        };
        let started = Instant::now();
        let p = run(&t);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // all zero bytes: binary, shown as an information table
        assert!(matches!(p, Preview::Info { .. }), "{p:?}");
    }

    #[test]
    fn binary_file_gives_an_info_table() {
        let d = TempDir::new().unwrap();
        let t = target(d.path(), "blob", &[0, 1, 2, 3, 255, 0, 9, 0]);
        match run(&t) {
            Preview::Info { rows, note } => {
                let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
                assert_eq!(labels, ["Type", "Size", "Modified", "Permissions"]);
                assert_eq!(note.as_deref(), Some("Binary file"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn utf16_text_with_nuls_is_still_text() {
        let d = TempDir::new().unwrap();
        let mut bytes = vec![0xff, 0xfe];
        for c in "hi there\n".encode_utf16() {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        let t = target(d.path(), "u16.txt", &bytes);
        match run(&t) {
            Preview::Text {
                lines, encoding, ..
            } => {
                assert_eq!(lines, ["hi there"]);
                assert_eq!(encoding, "utf-16le");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_file_and_parent_row_are_messages() {
        let d = TempDir::new().unwrap();
        let t = target(d.path(), "e", b"");
        assert!(matches!(run(&t), Preview::Message(m) if m == "Empty file"));
        let up = Target {
            name: "..".into(),
            ..t
        };
        assert!(matches!(run(&up), Preview::Message(m) if m == "Parent directory"));
    }

    #[test]
    fn special_and_broken_items_are_never_opened() {
        let d = TempDir::new().unwrap();
        // a FIFO: opening it for reading would block forever
        let fifo = d.path().join("pipe");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status();
        if status.is_ok_and(|s| s.success()) {
            let t = Target {
                path: fifo,
                name: "pipe".into(),
                kind: EntryKind::Other,
                size: 0,
                mtime_ns: 0,
                mode: 0o010_644,
            };
            assert!(matches!(run(&t), Preview::Info { .. }));
        }
        let t = Target {
            path: d.path().join("dangling"),
            name: "dangling".into(),
            kind: EntryKind::Symlink { target: None },
            size: 0,
            mtime_ns: 0,
            mode: 0o120_777,
        };
        assert!(matches!(run(&t), Preview::Info { .. }));
    }

    #[test]
    fn non_utf8_names_work() {
        let d = TempDir::new().unwrap();
        let path = d.path().join(OsStr::from_bytes(b"\xff\xfe.txt"));
        std::fs::write(&path, b"hello\n").unwrap();
        let t = Target {
            path,
            name: OsStr::from_bytes(b"\xff\xfe.txt").to_owned(),
            kind: EntryKind::File,
            size: 6,
            mtime_ns: 0,
            mode: 0o100_644,
        };
        assert!(matches!(run(&t), Preview::Text { .. }));
    }

    #[test]
    fn png_is_decoded_or_falls_back() {
        // a valid 2x1 PNG
        let png: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x7b, 0x40, 0xe8, 0xdd, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe,
            0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let d = TempDir::new().unwrap();
        let t = target(d.path(), "tiny.png", png);
        match run(&t) {
            Preview::Image(img) => {
                assert_eq!((img.width, img.height), (2, 1));
                assert!(img.caption.contains("PNG"), "{}", img.caption);
                assert!(img.bytes.len() >= 6);
            }
            // gdk-pixbuf without a PNG loader (minimal containers): information table
            Preview::Info { .. } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn image_dimension_cap() {
        assert!(image_dims_ok(4000, 3000));
        assert!(image_dims_ok(10_000, 10_000));
        assert!(!image_dims_ok(60_000, 60_000));
        assert!(!image_dims_ok(30_001, 1));
        assert!(!image_dims_ok(11_000, 10_000));
        assert!(!image_dims_ok(0, 5));
    }

    /// A PNG whose IHDR declares 60000x60000: only the header exists, so a decode attempt
    /// would fail with an error preview instead of the information table with the dimensions.
    #[test]
    fn huge_declared_png_shows_info_with_dimensions() {
        let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&60_000u32.to_be_bytes());
        png.extend_from_slice(&60_000u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        let d = TempDir::new().unwrap();
        let t = target(d.path(), "huge.png", &png);
        let Preview::Info { rows, note } = run(&t) else {
            panic!("expected an information table");
        };
        // Needs the gdk-pixbuf PNG loader (present wherever the decode test above yields an image).
        if Pixbuf::file_info(&t.path).is_some() {
            let dims = rows.iter().find(|r| r.label == tr("Dimensions")).unwrap();
            assert!(dims.value.contains("60000"), "{}", dims.value);
            assert_eq!(note.as_deref(), Some(tr("Image too large to preview")));
        }
    }

    #[test]
    fn symlinked_image_uses_the_opened_files_size() {
        let d = TempDir::new().unwrap();
        let real = d.path().join("real.dat");
        std::fs::write(&real, vec![0u8; 5000]).unwrap();
        let link = d.path().join("l.png");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(opened_file_size(&link), Some(5000));
        assert_eq!(opened_file_size(d.path()), None);
    }

    #[test]
    fn broken_image_falls_back_to_text_or_info() {
        let d = TempDir::new().unwrap();
        let t = target(d.path(), "bad.png", b"this is not a png");
        assert!(matches!(
            run(&t),
            Preview::Text { .. } | Preview::Info { .. }
        ));
    }

    #[test]
    fn cancelled_request_returns_quickly() {
        let d = TempDir::new().unwrap();
        let t = target(d.path(), "a.txt", b"x\n");
        let c = CancelToken::new();
        c.cancel();
        let p = build(&t, VIEW, &StatusFormat::default(), &c);
        assert!(matches!(p, Preview::Message(m) if m.is_empty()));
    }

    #[test]
    fn dir_size_reports_totals_and_can_be_cancelled() {
        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("a"), vec![0u8; 1000]).unwrap();
        std::fs::write(d.path().join("sub/b"), vec![0u8; 24]).unwrap();
        let (tx, rx) = async_channel::unbounded();
        dir_size(d.path(), &CancelToken::new(), 5, &tx);
        let mut done = None;
        while let Ok(m) = rx.try_recv() {
            assert_eq!(m.generation, 5);
            if let Update::DirDone(t) = m.update {
                done = Some(t);
            }
        }
        let t = done.expect("DirDone");
        assert_eq!((t.files, t.dirs, t.bytes), (2, 2, 1024));

        let cancel = CancelToken::new();
        cancel.cancel();
        let (tx, rx) = async_channel::unbounded();
        dir_size(d.path(), &cancel, 6, &tx);
        assert!(rx.try_recv().is_err(), "a cancelled walk reports nothing");
    }

    #[test]
    fn helpers() {
        assert!(is_image_name(OsStr::new("A.PNG")));
        assert!(!is_image_name(OsStr::new("png")));
        assert_eq!(mode_string(0o100_644), "-rw-r--r--");
        assert_eq!(mode_string(0o040_755), "drwxr-xr-x");
        assert_eq!(mode_string(0o104_755), "-rwsr-xr-x");
        assert_eq!(mode_string(0o041_777), "drwxrwxrwt");
        assert_eq!(sanitize_line("a\tb\u{1}c"), "a       b\u{b7}c");
        let long = "x".repeat(1000);
        let s = sanitize_line(&long);
        assert_eq!(s.chars().count(), MAX_LINE_CHARS + 1);
        assert!(s.ends_with('\u{2026}'));
        assert!(looks_binary(&[1, 0, 2], Encoding::Utf8, 0));
        assert!(!looks_binary(b"plain\n", Encoding::Utf8, 0));
        assert!(!looks_binary(&[0, b'a'], Encoding::Utf16Le, 0));
    }
}
