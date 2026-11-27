//! Zip passwords that pass the weak header check but do not decrypt (F26).
//!
//! ZipCrypto compares one header byte, so about 1 in 256 wrong passwords look right until the CRC.
//! The tests search for such a password for each fixture, which makes the case deterministic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ofm_core::CancelToken;
use ofm_vfs::archive::{
    ArchiveError, ArchiveLimits, ArchiveReader, EntryId, MAX_PASSWORD_ATTEMPTS, PasswordProvider,
    PasswordSource, open_archive,
};
use tempfile::TempDir;
use zip::unstable::write::FileOptionsExt;
use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, ZipArchive, ZipWriter};

/// Answers from a script (then `None`) and records what happened.
#[derive(Default)]
struct Script {
    answers: Mutex<Vec<String>>,
    asked: Mutex<Vec<bool>>,
    accepted: Mutex<Vec<String>>,
}

impl Script {
    fn new(answers: &[&str]) -> Self {
        Script {
            answers: Mutex::new(answers.iter().rev().map(|a| (*a).to_owned()).collect()),
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
        self.answers.lock().unwrap().pop()
    }
    fn accepted(&self, _: &Path, password: &str) {
        self.accepted.lock().unwrap().push(password.to_owned());
    }
}

#[derive(Clone, Copy)]
enum Cipher {
    ZipCrypto(CompressionMethod),
    Aes,
}

fn text(lines: u32) -> Vec<u8> {
    (0..lines)
        .flat_map(|i| format!("line {i}\n").into_bytes())
        .collect()
}

/// A zip with every file encrypted with "secret".
fn archive(cipher: Cipher, files: &[(&str, &[u8])]) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.zip");
    let mut writer = ZipWriter::new(File::create(&path).unwrap());
    for (name, data) in files {
        let base = SimpleFileOptions::default();
        let options = match cipher {
            Cipher::ZipCrypto(method) => base
                .compression_method(method)
                .with_deprecated_encryption(b"secret")
                .unwrap(),
            Cipher::Aes => base
                .compression_method(CompressionMethod::Stored)
                .with_aes_encryption(AesMode::Aes256, "secret"),
        };
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
    (dir, path)
}

/// A wrong password that the header check of entry `index` accepts.
fn colliding_password(path: &Path, index: usize) -> String {
    let mut zip = ZipArchive::new(File::open(path).unwrap()).unwrap();
    (0..100_000)
        .map(|i| format!("wrong{i}"))
        .find(|p| {
            let Ok(mut file) = zip.by_index_decrypt(index, p.as_bytes()) else {
                return false;
            };
            // It must really be wrong: the data does not survive.
            std::io::copy(&mut file, &mut std::io::sink()).is_err()
        })
        .expect("about one in 256 passwords passes the header check")
}

/// Flip one byte in the encrypted data of entry `index`, after the encryption header.
fn damage(path: &Path, index: usize) {
    let start = {
        let mut zip = ZipArchive::new(File::open(path).unwrap()).unwrap();
        zip.by_index_raw(index).unwrap().data_start().unwrap()
    };
    let mut bytes = std::fs::read(path).unwrap();
    // 20 is past the ZipCrypto header (12) and the AES-256 salt and check value (18).
    bytes[start as usize + 20] ^= 0xff;
    std::fs::write(path, bytes).unwrap();
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

type Delivered = Vec<(PathBuf, Vec<u8>)>;

fn visit(reader: &mut dyn ArchiveReader, names: &[&str]) -> (Delivered, Result<(), ArchiveError>) {
    let ids: Vec<EntryId> = names
        .iter()
        .map(|n| reader.index().lookup(Path::new(n)).unwrap().id)
        .collect();
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

const METHODS: [CompressionMethod; 2] = [CompressionMethod::Stored, CompressionMethod::Deflated];

#[test]
fn a_password_passing_only_the_header_check_prompts_again() {
    for method in METHODS {
        let data = text(500);
        let (_tmp, path) = archive(Cipher::ZipCrypto(method), &[("s.txt", &data)]);
        let wrong = colliding_password(&path, 0);
        let passwords = Script::new(&[&wrong, "secret"]);
        let mut reader = open(&path, &passwords);
        let (seen, result) = visit(&mut *reader, &["s.txt"]);
        result.unwrap();
        assert_eq!(seen, [one("s.txt", &data)], "{method:?}");
        assert_eq!(passwords.asked(), [false, true]);
        assert_eq!(passwords.accepted(), ["secret"]);
    }
}

#[test]
fn a_declined_retry_after_such_a_password_is_a_wrong_password() {
    for method in METHODS {
        let (_tmp, path) = archive(Cipher::ZipCrypto(method), &[("s.txt", &text(500))]);
        let wrong = colliding_password(&path, 0);
        let passwords = Script::new(&[&wrong]);
        let mut reader = open(&path, &passwords);
        let (seen, result) = visit(&mut *reader, &["s.txt"]);
        assert!(
            matches!(result, Err(ArchiveError::WrongPassword(_))),
            "{result:?}"
        );
        assert!(seen.is_empty(), "no garbage reaches the visitor");
        assert!(passwords.accepted().is_empty());
    }
}

#[test]
fn a_stale_cached_password_passing_the_header_check_is_forgotten() {
    let (_tmp, path) = archive(
        Cipher::ZipCrypto(CompressionMethod::Stored),
        &[("s.txt", b"classified")],
    );
    let inner = Arc::new(Script::new(&["secret"]));
    let source = PasswordSource::new(inner.clone());
    source.accepted(&path, &colliding_password(&path, 0));
    let mut reader = open(&path, &source);
    let (seen, result) = visit(&mut *reader, &["s.txt"]);
    result.unwrap();
    assert_eq!(seen, [one("s.txt", b"classified")]);
    assert_eq!(inner.asked(), [true]);
    assert_eq!(source.password(&path, false).as_deref(), Some("secret"));
}

#[test]
fn a_damaged_entry_ends_in_an_error_after_bounded_prompts() {
    for cipher in [
        Cipher::ZipCrypto(CompressionMethod::Stored),
        Cipher::ZipCrypto(CompressionMethod::Deflated),
        Cipher::Aes,
    ] {
        let (_tmp, path) = archive(cipher, &[("s.txt", &text(500))]);
        damage(&path, 0);
        // The user insists on the right password for ever.
        let passwords = Script::new(&["secret"; 10]);
        let mut reader = open(&path, &passwords);
        let (seen, result) = visit(&mut *reader, &["s.txt"]);
        assert!(
            matches!(result, Err(ArchiveError::Corrupt { .. })),
            "{result:?}"
        );
        assert!(seen.is_empty());
        assert_eq!(passwords.asked().len(), MAX_PASSWORD_ATTEMPTS as usize);
        assert!(passwords.accepted().is_empty());
    }
}

#[test]
fn a_damaged_entry_does_not_lock_the_others() {
    for cipher in [Cipher::ZipCrypto(CompressionMethod::Stored), Cipher::Aes] {
        let good = text(500);
        // The damaged entry is the smallest, so the password is tried on it first.
        let (_tmp, path) = archive(cipher, &[("good", &good), ("bad", &text(20))]);
        damage(&path, 1);
        let passwords = Script::new(&["secret"]);
        let mut reader = open(&path, &passwords);
        let (seen, result) = visit(&mut *reader, &["good", "bad"]);
        assert!(
            matches!(result, Err(ArchiveError::Corrupt { .. })),
            "{result:?}"
        );
        assert_eq!(seen[0], one("good", &good));
        assert_eq!(passwords.asked(), [false]);
        assert_eq!(passwords.accepted(), ["secret"]);
    }
}
