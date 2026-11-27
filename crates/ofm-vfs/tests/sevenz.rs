//! 7z passwords, symlinks and selective decoding (F26), on archives written by the library.
//!
//! Encrypted fixtures list the coders as 7-Zip chains them: compress, then encrypt. The writer
//! applies `methods[0]` last (it is the one decoding starts with), hence `[AES, compressor]`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ofm_core::CancelToken;
use ofm_vfs::archive::{
    ArchiveEntryKind, ArchiveError, ArchiveLimits, ArchiveReader, EntryId, EntrySafety,
    MAX_PASSWORD_ATTEMPTS, NoPassword, PasswordProvider, PasswordSource, open_archive,
};
use sevenz_rust2::encoder_options::{AesEncoderOptions, EncoderOptions};
use sevenz_rust2::{
    ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod, Password, SourceReader,
};
use tempfile::TempDir;

/// Answers from a script (then `None`) and records what happened.
#[derive(Default)]
struct Script {
    answers: Mutex<Vec<&'static str>>,
    asked: Mutex<Vec<bool>>,
    accepted: Mutex<Vec<String>>,
}

impl Script {
    fn new(answers: &[&'static str]) -> Self {
        Script {
            answers: Mutex::new(answers.iter().rev().copied().collect()),
            ..Script::default()
        }
    }
    fn asked(&self) -> Vec<bool> {
        self.asked.lock().unwrap().clone()
    }
    fn accepted(&self) -> Vec<String> {
        self.accepted.lock().unwrap().clone()
    }
}

impl PasswordProvider for Script {
    fn password(&self, _: &Path, retry: bool) -> Option<String> {
        self.asked.lock().unwrap().push(retry);
        self.answers.lock().unwrap().pop().map(str::to_owned)
    }
    fn accepted(&self, _: &Path, password: &str) {
        self.accepted.lock().unwrap().push(password.to_owned());
    }
}

fn plain(method: EncoderMethod) -> Vec<EncoderConfiguration> {
    vec![EncoderConfiguration::new(method)]
}

fn aes(method: EncoderMethod) -> Vec<EncoderConfiguration> {
    vec![
        EncoderConfiguration::new(EncoderMethod::AES256_SHA256).with_options(EncoderOptions::Aes(
            AesEncoderOptions::new(Password::from("secret")),
        )),
        EncoderConfiguration::new(method),
    ]
}

fn file(name: &str) -> ArchiveEntry {
    ArchiveEntry {
        name: name.to_owned(),
        has_stream: true,
        ..ArchiveEntry::default()
    }
}

fn symlink(name: &str) -> ArchiveEntry {
    ArchiveEntry {
        has_windows_attributes: true,
        windows_attributes: 0x8000 | (0o120777 << 16),
        ..file(name)
    }
}

fn body() -> Vec<u8> {
    (0..20_000u32)
        .flat_map(|i| format!("line {i}\n").into_bytes())
        .collect()
}

/// Write an archive with plain headers unless `build` says otherwise.
fn archive(build: impl FnOnce(&mut ArchiveWriter<std::fs::File>)) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.7z");
    let mut writer = ArchiveWriter::create(&path).unwrap();
    writer.set_encrypt_header(false);
    build(&mut writer);
    writer.finish().unwrap();
    (dir, path)
}

fn add(writer: &mut ArchiveWriter<std::fs::File>, entry: ArchiveEntry, data: &[u8]) {
    writer.push_archive_entry(entry, Some(data)).unwrap();
}

fn add_solid(writer: &mut ArchiveWriter<std::fs::File>, files: &[(ArchiveEntry, &[u8])]) {
    let entries = files.iter().map(|(e, _)| e.clone()).collect();
    let readers = files.iter().map(|(_, d)| SourceReader::new(*d)).collect();
    writer.push_archive_entries(entries, readers).unwrap();
}

fn open<'p>(path: &Path, passwords: &'p dyn PasswordProvider) -> Box<dyn ArchiveReader + 'p> {
    open_archive(
        path,
        passwords,
        &ArchiveLimits::default(),
        &CancelToken::new(),
    )
    .unwrap()
}

fn id(reader: &dyn ArchiveReader, name: &str) -> EntryId {
    reader.index().lookup(Path::new(name)).unwrap().id
}

type Delivered = Vec<(PathBuf, Vec<u8>)>;

/// Visit `names`, returning what the visitor saw (also on failure). Read errors are swallowed by
/// the visitor on purpose: the backend must not hand it garbage in the first place, and must
/// still report the failure.
fn visit(reader: &mut dyn ArchiveReader, names: &[&str]) -> (Delivered, Result<(), ArchiveError>) {
    let ids: Vec<EntryId> = names.iter().map(|n| id(reader, n)).collect();
    let mut seen = Vec::new();
    let result = reader.visit(&ids, &CancelToken::new(), &mut |entry, data| {
        let mut bytes = Vec::new();
        let _ = data.read_to_end(&mut bytes);
        seen.push((entry.path.clone(), bytes));
        Ok(())
    });
    (seen, result)
}

fn one(path: &str, data: &[u8]) -> (PathBuf, Vec<u8>) {
    (PathBuf::from(path), data.to_vec())
}

#[test]
fn a_wrong_password_prompts_again_before_any_data_is_delivered() {
    for method in [
        EncoderMethod::LZMA2,
        EncoderMethod::COPY,
        EncoderMethod::LZMA,
    ] {
        let data = body();
        let (_tmp, path) = archive(|w| {
            w.set_content_methods(aes(method));
            add(w, file("s.txt"), &data);
        });
        let passwords = Script::new(&["wrong", "secret"]);
        let mut reader = open(&path, &passwords);
        assert_eq!(passwords.asked(), Vec::<bool>::new(), "listing never asks");
        assert!(reader.index().entries()[0].encrypted);
        let (seen, result) = visit(&mut *reader, &["s.txt"]);
        result.unwrap();
        assert_eq!(seen, [one("s.txt", &data)], "{method:?}");
        assert_eq!(passwords.asked(), [false, true]);
        assert_eq!(passwords.accepted(), ["secret"]);

        // The reader keeps the verified password.
        let (seen, result) = visit(&mut *reader, &["s.txt"]);
        result.unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(passwords.asked(), [false, true]);
    }
}

#[test]
fn wrong_passwords_are_bounded_and_deliver_nothing() {
    let (_tmp, path) = archive(|w| {
        w.set_content_methods(aes(EncoderMethod::COPY));
        add(w, file("s.txt"), b"classified");
    });
    let passwords = Script::new(&["a", "", "c", "secret"]);
    let mut reader = open(&path, &passwords);
    let (seen, result) = visit(&mut *reader, &["s.txt"]);
    assert!(matches!(result, Err(ArchiveError::WrongPassword(_))));
    assert!(seen.is_empty(), "no garbage reaches the visitor");
    assert_eq!(passwords.asked().len(), MAX_PASSWORD_ATTEMPTS as usize);
    assert!(passwords.accepted().is_empty());
}

#[test]
fn a_stale_cached_password_is_forgotten() {
    let (_tmp, path) = archive(|w| {
        w.set_content_methods(aes(EncoderMethod::LZMA2));
        add(w, file("s.txt"), b"classified");
    });
    let inner = Arc::new(Script::new(&["secret"]));
    let source = PasswordSource::new(inner.clone());
    source.accepted(&path, "stale");
    let mut reader = open(&path, &source);
    let (seen, result) = visit(&mut *reader, &["s.txt"]);
    result.unwrap();
    assert_eq!(seen, [one("s.txt", b"classified")]);
    assert_eq!(
        inner.asked(),
        [true],
        "the cache answered the first question"
    );
    assert_eq!(source.password(&path, false).as_deref(), Some("secret"));
}

#[test]
fn tiny_first_entries_do_not_verify_a_password_alone() {
    let (_tmp, path) = archive(|w| {
        w.set_content_methods(aes(EncoderMethod::COPY));
        add_solid(
            w,
            &[(file("a"), b"1"), (file("b"), b"two"), (file("c"), b"3")],
        );
    });
    let passwords = Script::new(&["wrong", "secret"]);
    let mut reader = open(&path, &passwords);
    let (seen, result) = visit(&mut *reader, &["c"]);
    result.unwrap();
    assert_eq!(seen, [one("c", b"3")]);
    assert_eq!(passwords.accepted(), ["secret"]);
}

fn mixed() -> (TempDir, PathBuf) {
    archive(|w| {
        w.set_content_methods(plain(EncoderMethod::LZMA2));
        add(w, file("plain.txt"), b"plain");
        w.set_content_methods(aes(EncoderMethod::LZMA2));
        add(w, file("s.txt"), &body());
    })
}

#[test]
fn mixed_archive_prompts_only_for_encrypted_entries() {
    let (_tmp, path) = mixed();

    // The plain entry alone: no question at all.
    let passwords = Script::new(&[]);
    let mut reader = open(&path, &passwords);
    let (seen, result) = visit(&mut *reader, &["plain.txt"]);
    result.unwrap();
    assert_eq!(seen, [one("plain.txt", b"plain")]);
    assert!(passwords.asked().is_empty());

    // Everything: asked even though the plain entry comes first.
    let passwords = Script::new(&["wrong", "secret"]);
    let mut reader = open(&path, &passwords);
    let (seen, result) = visit(&mut *reader, &["plain.txt", "s.txt"]);
    result.unwrap();
    assert_eq!(seen, [one("plain.txt", b"plain"), one("s.txt", &body())]);
    assert_eq!(passwords.asked(), [false, true]);
}

#[test]
fn declining_the_prompt_delivers_nothing() {
    let (_tmp, path) = mixed();
    let mut reader = open(&path, &NoPassword);
    let (seen, result) = visit(&mut *reader, &["plain.txt", "s.txt"]);
    assert!(matches!(result, Err(ArchiveError::NeedsPassword(_))));
    assert!(seen.is_empty());
}

#[test]
fn cancel_while_prompting_is_cancelled() {
    struct CancelPrompt(CancelToken);
    impl PasswordProvider for CancelPrompt {
        fn password(&self, _: &Path, _: bool) -> Option<String> {
            self.0.cancel();
            None
        }
    }
    let (_tmp, path) = mixed();
    let cancel = CancelToken::new();
    let passwords = CancelPrompt(cancel.clone());
    let mut reader = open(&path, &passwords);
    let secret = id(&*reader, "s.txt");
    let result = reader.visit(&[secret], &cancel, &mut |_, _| Ok(()));
    assert!(matches!(result, Err(ArchiveError::Cancelled)));
}

#[test]
fn cancel_from_the_visitor_stops_the_walk() {
    let (_tmp, path) = archive(|w| {
        add_solid(w, &[(file("a"), &body()), (file("b"), b"bb")]);
    });
    let mut reader = open(&path, &NoPassword);
    let ids = [id(&*reader, "a"), id(&*reader, "b")];
    let cancel = CancelToken::new();
    let mut calls = 0;
    let result = reader.visit(&ids, &cancel, &mut |_, _| {
        calls += 1;
        cancel.cancel();
        Ok(())
    });
    assert!(matches!(result, Err(ArchiveError::Cancelled)));
    assert_eq!(calls, 1);
}

/// Flip a byte of the first packed stream (stored data begins right after the 32-byte signature
/// header).
fn corrupt_packed_byte(path: &Path, offset: usize) {
    let mut bytes = std::fs::read(path).unwrap();
    bytes[32 + offset] ^= 0xff;
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn nothing_after_the_last_wanted_entry_is_decoded() {
    let (_tmp, path) = archive(|w| {
        w.set_content_methods(plain(EncoderMethod::COPY));
        add_solid(w, &[(file("a"), b"first"), (file("b"), b"second")]);
    });
    corrupt_packed_byte(&path, 7);
    let mut reader = open(&path, &NoPassword);
    let (seen, result) = visit(&mut *reader, &["a"]);
    result.unwrap();
    assert_eq!(seen, [one("a", b"first")]);
    // The damage is still reported when the damaged entry is wanted, even if the visitor
    // ignores the read error.
    let (_, result) = visit(&mut *reader, &["b"]);
    assert!(matches!(result, Err(ArchiveError::Corrupt { .. })));
}

#[test]
fn empty_files_and_directories_are_delivered_after_the_data() {
    let (_tmp, path) = archive(|w| {
        w.push_archive_entry(
            ArchiveEntry {
                name: "d".to_owned(),
                is_directory: true,
                ..ArchiveEntry::default()
            },
            None::<&[u8]>,
        )
        .unwrap();
        w.push_archive_entry(
            ArchiveEntry {
                name: "empty".to_owned(),
                ..ArchiveEntry::default()
            },
            None::<&[u8]>,
        )
        .unwrap();
        add(w, file("d/f"), b"data");
    });
    let mut reader = open(&path, &NoPassword);
    let (seen, result) = visit(&mut *reader, &["d", "empty", "d/f"]);
    result.unwrap();
    assert_eq!(
        seen,
        [one("d/f", b"data"), one("d", b""), one("empty", b"")]
    );
}

fn with_symlink(encrypt_header: bool) -> (TempDir, PathBuf) {
    archive(|w| {
        w.set_encrypt_header(encrypt_header);
        w.set_content_methods(aes(EncoderMethod::LZMA2));
        add(w, file("s.txt"), &body());
        add(w, symlink("l"), b"s.txt");
    })
}

#[test]
fn plain_symlinks_get_their_target() {
    let (_tmp, path) = archive(|w| {
        add_solid(w, &[(file("s.txt"), b"data"), (symlink("l"), b"s.txt")]);
    });
    let reader = open(&path, &NoPassword);
    let link = reader.index().lookup(Path::new("l")).unwrap();
    assert_eq!(link.kind, ArchiveEntryKind::Symlink);
    assert_eq!(link.link_target.as_deref(), Some(Path::new("s.txt")));
    assert_eq!(link.safety, EntrySafety::Safe);
}

#[test]
fn symlink_indexing_stops_after_the_last_symlink() {
    let (_tmp, path) = archive(|w| {
        w.set_content_methods(plain(EncoderMethod::COPY));
        add_solid(w, &[(symlink("l"), b"z"), (file("z"), b"payload")]);
    });
    corrupt_packed_byte(&path, 3);
    let reader = open(&path, &NoPassword);
    let link = reader.index().lookup(Path::new("l")).unwrap();
    assert_eq!(link.link_target.as_deref(), Some(Path::new("z")));
}

#[test]
fn an_encrypted_symlink_with_plain_headers_does_not_block_opening() {
    let (_tmp, path) = with_symlink(false);
    let passwords = Script::new(&["secret"]);
    let mut reader = open(&path, &passwords);
    assert!(
        passwords.asked().is_empty(),
        "indexing never prompts for data"
    );
    // Without its target the link cannot be proven safe.
    assert_eq!(reader.index().unsafe_entries().count(), 1);
    assert!(reader.index().lookup(Path::new("l")).is_none());
    let (seen, result) = visit(&mut *reader, &["s.txt"]);
    result.unwrap();
    assert_eq!(seen, [one("s.txt", &body())]);

    // Searches and listings without any password still work.
    open(&path, &NoPassword);
}

#[test]
fn encrypted_headers_give_the_password_for_symlink_targets() {
    let (_tmp, path) = with_symlink(true);
    let passwords = Script::new(&["wrong", "secret"]);
    let mut reader = open(&path, &passwords);
    assert_eq!(passwords.asked(), [false, true]);
    let link = reader.index().lookup(Path::new("l")).unwrap();
    assert_eq!(link.link_target.as_deref(), Some(Path::new("s.txt")));
    assert_eq!(link.safety, EntrySafety::Safe);
    let (seen, result) = visit(&mut *reader, &["s.txt", "l"]);
    result.unwrap();
    assert_eq!(seen[0], one("s.txt", &body()));
    assert_eq!(seen.len(), 2);
    assert_eq!(passwords.asked(), [false, true], "asked once per password");
}

#[test]
fn a_damaged_block_does_not_lock_the_others() {
    let good = body();
    let (_tmp, path) = archive(|w| {
        w.set_content_methods(aes(EncoderMethod::COPY));
        add(w, file("good"), &good);
        // The smallest block, so the password is tried on it first.
        add(w, file("bad"), &[7; 100]);
    });
    let first = sevenz_rust2::Archive::open(&path).unwrap().pack_sizes()[0];
    corrupt_packed_byte(&path, first as usize + 40);

    let passwords = Script::new(&["secret"]);
    let mut reader = open(&path, &passwords);
    let (seen, result) = visit(&mut *reader, &["good", "bad"]);
    assert!(
        matches!(result, Err(ArchiveError::Corrupt { .. })),
        "{result:?}"
    );
    assert_eq!(seen[0], one("good", &good));
    assert_eq!(passwords.asked(), [false]);

    // On its own it cannot be told from a wrong password: bounded, then a clear error.
    let passwords = Script::new(&["secret"; 10]);
    let mut reader = open(&path, &passwords);
    let (seen, result) = visit(&mut *reader, &["bad"]);
    assert!(
        matches!(result, Err(ArchiveError::WrongPassword(_))),
        "{result:?}"
    );
    assert!(seen.is_empty());
    assert_eq!(passwords.asked().len(), MAX_PASSWORD_ATTEMPTS as usize);
}
