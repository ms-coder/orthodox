//! T6.2 extract job against REAL archives made by the `zip`/`tar`/`gzip`/`xz`/`zstd`/`bzip2`/`7z`
//! command line tools and by the tiny hand-rolled zip/tar writers below (hostile layouts no
//! well-behaved tool will produce). Everything skips politely when the format backend of
//! `ofm-vfs` is still a stub (`ArchiveError::NotImplemented`) or a tool is not installed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod extract_support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use extract_support::{Tmp, assert_absent, assert_no_part_files, read, snapshot};
use ofm_core::CancelToken;
use ofm_jobs::{ExtractOpts, JobSpec, Outcome, RealFs, ScriptedSink, run_sync};
use ofm_testkit::compare::{Compare, assert_trees_equal};
use ofm_testkit::tree::{FileSpec, TreeSpec};
use ofm_vfs::archive::{
    ArchiveError, ArchiveLimits, FixedPassword, NoPassword, PasswordSource, open_archive,
};

// ------------------------------------------------------------------------------- helpers

fn have(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// False (with a note) while the backends are stubs.
fn backend_ready(archive: &Path) -> bool {
    match open_archive(
        archive,
        &NoPassword,
        &ArchiveLimits::default(),
        &CancelToken::new(),
    ) {
        Err(ArchiveError::NotImplemented(what)) => {
            eprintln!("SKIP: archive backend not implemented yet ({what})");
            false
        }
        _ => true,
    }
}

fn sh(dir: &Path, cmd: &str) {
    let out = Command::new("sh")
        .current_dir(dir)
        .args(["-c", cmd])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "`{cmd}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A source tree with nested dirs, empty files, modes, a symlink, a hard link and awkward names.
fn source_tree(root: &Path) {
    TreeSpec::new()
        .file("top.txt", FileSpec::content(b"top level\n"))
        .file("run.sh", FileSpec::content(b"#!/bin/sh\n").mode(0o755))
        .file("empty", FileSpec::content(b""))
        .file("sp ace.txt", FileSpec::content(b"spaces"))
        .file("-dash.txt", FileSpec::content(b"dash"))
        .file("ünï.txt", FileSpec::content(b"unicode"))
        .dir(
            "sub",
            TreeSpec::new()
                .file("a.bin", FileSpec::size(100_000))
                .file("b.txt", FileSpec::content(b"bee"))
                .dir(
                    "deeper",
                    TreeSpec::new().file("c.txt", FileSpec::content(b"sea")),
                ),
        )
        .symlink("link", "top.txt")
        .symlink("sub/up", "../top.txt")
        .hardlink("hard.txt", "top.txt")
        .build(root)
        .unwrap();
}

fn extract(archive: &Path, entries: &[&str], dst: &Path, opts: ExtractOpts) -> Outcome {
    let (sink, _) = ScriptedSink::refusing();
    run_sync(
        &JobSpec::Extract {
            archive: archive.to_path_buf(),
            entries: entries.iter().map(PathBuf::from).collect(),
            dest_dir: dst.to_path_buf(),
            opts,
        },
        Arc::new(RealFs),
        sink,
    )
}

fn content_and_mode() -> Compare {
    // Archives store seconds (zip: 2 s DOS time) and no xattrs.
    Compare {
        content: true,
        mode: true,
        mtime: false,
        xattrs: false,
    }
}

// ------------------------------------------------------------- real archives from tools

#[test]
fn zip_made_by_info_zip_extracts_to_the_same_tree() {
    if !have("zip") {
        eprintln!("SKIP: no zip");
        return;
    }
    let tmp = Tmp::new("r-zip");
    let src = tmp.path().join("src");
    source_tree(&src);
    let archive = tmp.path().join("t.zip");
    sh(&src, &format!("zip -q -r -y {} .", archive.display()));
    if !backend_ready(&archive) {
        return;
    }
    let dst = tmp.dst();
    let out = extract(&archive, &[], &dst, ExtractOpts::default());
    assert!(out.is_success(), "{out:?}");
    // zip stores no hard links: `hard.txt` is a normal file with the same content.
    assert_eq!(read(&dst.join("hard.txt")), b"top level\n");
    std::fs::remove_file(dst.join("hard.txt")).unwrap();
    std::fs::remove_file(src.join("hard.txt")).unwrap();
    assert_trees_equal(&src, &dst, content_and_mode());
    assert_no_part_files(&dst);
}

fn tar_variants() -> Vec<(&'static str, &'static str, &'static str)> {
    // (extension, tool needed, tar flags)
    vec![
        ("tar", "tar", "-cf"),
        ("tar.gz", "gzip", "-czf"),
        ("tar.bz2", "bzip2", "-cjf"),
        ("tar.xz", "xz", "-cJf"),
        ("tar.zst", "zstd", "--zstd -cf"),
    ]
}

#[test]
fn tar_family_extracts_to_the_same_tree_with_links() {
    for (ext, tool, flags) in tar_variants() {
        if !have(tool) {
            eprintln!("SKIP {ext}: no {tool}");
            continue;
        }
        let tmp = Tmp::new("r-tar");
        let src = tmp.path().join("src");
        source_tree(&src);
        let archive = tmp.path().join(format!("t.{ext}"));
        sh(&src, &format!("tar {flags} {} .", archive.display()));
        if !backend_ready(&archive) {
            return;
        }
        let dst = tmp.dst();
        let out = extract(&archive, &[], &dst, ExtractOpts::default());
        assert!(out.is_success(), "{ext}: {out:?}");
        assert_trees_equal(&src, &dst, content_and_mode());
        // The hard link pair is one inode again.
        let ino =
            |p: &str| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(dst.join(p)).unwrap());
        assert_eq!(ino("hard.txt"), ino("top.txt"), "{ext}");
        assert_no_part_files(&dst);
    }
}

#[test]
fn a_subdirectory_of_a_real_tar_gz_can_be_extracted_alone() {
    if !have("tar") || !have("gzip") {
        return;
    }
    let tmp = Tmp::new("r-sel");
    let src = tmp.path().join("src");
    source_tree(&src);
    let archive = tmp.path().join("t.tar.gz");
    sh(&src, &format!("tar -czf {} .", archive.display()));
    if !backend_ready(&archive) {
        return;
    }
    let dst = tmp.dst();
    let out = extract(&archive, &["sub/deeper"], &dst, ExtractOpts::default());
    assert!(out.is_success(), "{out:?}");
    let names: Vec<_> = snapshot(&dst).into_iter().map(|(p, _)| p).collect();
    assert_eq!(
        names,
        [PathBuf::from("deeper"), PathBuf::from("deeper/c.txt")]
    );
}

#[test]
fn seven_zip_archives_extract() {
    let tool = ["7z", "7zz", "7za"].into_iter().find(|t| have(t));
    let Some(tool) = tool else {
        eprintln!("SKIP: no 7z tool");
        return;
    };
    let tmp = Tmp::new("r-7z");
    let src = tmp.path().join("src");
    source_tree(&src);
    let archive = tmp.path().join("t.7z");
    sh(
        &src,
        &format!("{tool} a -bd -snl {} . >/dev/null", archive.display()),
    );
    if !backend_ready(&archive) {
        return;
    }
    let dst = tmp.dst();
    let out = extract(&archive, &[], &dst, ExtractOpts::default());
    assert!(out.is_success(), "{out:?}");
    assert_eq!(read(&dst.join("sub/deeper/c.txt")), b"sea");
    assert_no_part_files(&dst);
}

// ------------------------------------------------------------------------------- passwords

#[test]
fn encrypted_zip_asks_for_the_password_and_reports_a_wrong_one() {
    if !have("zip") {
        return;
    }
    let tmp = Tmp::new("r-pw");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("secret.txt"), b"classified").unwrap();
    let archive = tmp.path().join("enc.zip");
    sh(
        &src,
        &format!("zip -q -P hunter2 {} secret.txt", archive.display()),
    );
    if !backend_ready(&archive) {
        return;
    }
    let with = |pw: &str| ExtractOpts {
        password: PasswordSource::new(Arc::new(FixedPassword(pw.into()))),
        ..ExtractOpts::default()
    };
    let dst = tmp.dst();
    let out = extract(&archive, &[], &dst, with("hunter2"));
    assert!(out.is_success(), "{out:?}");
    assert_eq!(read(&dst.join("secret.txt")), b"classified");

    let dst2 = tmp.path().join("dst2");
    let out = extract(&archive, &[], &dst2, with("wrong"));
    assert!(
        matches!(&out, Outcome::Failed { error, .. } if error.contains("wrong password")),
        "{out:?}"
    );
    let dst3 = tmp.path().join("dst3");
    let out = extract(&archive, &[], &dst3, ExtractOpts::default());
    assert!(
        matches!(&out, Outcome::Failed { error, .. } if error.contains("password required")),
        "{out:?}"
    );
    for d in [&dst2, &dst3] {
        let leftover: Vec<_> = snapshot(d)
            .into_iter()
            .filter(|(p, _)| p.ends_with("secret.txt"))
            .collect();
        assert!(leftover.is_empty(), "{leftover:?}");
        assert_no_part_files(d);
    }
}

// ------------------------------------------------------------------------------- corruption

#[test]
fn damaged_archives_fail_or_warn_but_never_crash_or_leave_temp_files() {
    if !have("tar") || !have("gzip") || !have("zip") {
        return;
    }
    let tmp = Tmp::new("r-corrupt");
    let src = tmp.path().join("src");
    source_tree(&src);
    let mut archives = Vec::new();
    for (name, cmd) in [("t.zip", "zip -q -r -y"), ("t.tar.gz", "tar -czf")] {
        let a = tmp.path().join(name);
        let cmd = format!("{cmd} {} .", a.display());
        sh(&src, &cmd);
        archives.push(a);
    }
    if !backend_ready(&archives[0]) {
        return;
    }
    for archive in archives {
        let good = std::fs::read(&archive).unwrap();
        // Truncated at many points, and with a flipped byte at many points.
        let mut variants: Vec<Vec<u8>> = Vec::new();
        for frac in [1, 2, 3, 5, 8, 9] {
            variants.push(good[..good.len() * frac / 10].to_vec());
        }
        for at in (0..good.len()).step_by((good.len() / 25).max(1)) {
            let mut bad = good.clone();
            bad[at] ^= 0xff;
            variants.push(bad);
        }
        for (i, bytes) in variants.iter().enumerate() {
            let bad = tmp.path().join(format!(
                "bad{i}{}",
                archive.file_name().unwrap().to_string_lossy()
            ));
            std::fs::write(&bad, bytes).unwrap();
            let dst = tmp.path().join(format!(
                "out{i}-{}",
                archive.file_name().unwrap().to_string_lossy()
            ));
            let out = extract(&bad, &[], &dst, ExtractOpts::default());
            // Ok (damage in unused bytes / per-entry warnings), Failed or Cancelled: anything but a
            // crash, a leftover temp file or a truncated final file.
            assert!(
                matches!(
                    out,
                    Outcome::Ok(_) | Outcome::Failed { .. } | Outcome::Cancelled(_)
                ),
                "variant {i}: {out:?}"
            );
            if dst.exists() {
                assert_no_part_files(&dst);
            }
        }
    }
}

// -------------------------------------------------------------- hostile archives, by hand

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A zip with stored (uncompressed) entries and arbitrary raw names.
fn raw_zip(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let off = out.len() as u32;
        let crc = crc32(data);
        let mut local = Vec::new();
        local.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        local.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]); // version, flags, method 0, time, date
        local.extend_from_slice(&crc.to_le_bytes());
        local.extend_from_slice(&(data.len() as u32).to_le_bytes());
        local.extend_from_slice(&(data.len() as u32).to_le_bytes());
        local.extend_from_slice(&(name.len() as u16).to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(name);
        out.extend_from_slice(&local);
        out.extend_from_slice(data);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&[20, 3, 20, 0, 0, 0, 0, 0, 0, 0, 0x21, 0]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]); // extra, comment, disk, internal attrs
        central.extend_from_slice(&0o100_644u32.wrapping_shl(16).to_le_bytes()); // external attrs
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// One ustar entry: `kind` is the typeflag (`b'0'` file, `b'2'` symlink, `b'1'` hard link,
/// `b'5'` dir, `b'6'` fifo, `b'3'` char device).
fn tar_entry(name: &[u8], kind: u8, link: &[u8], data: &[u8]) -> Vec<u8> {
    let mut h = [0u8; 512];
    h[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
    h[136..148].copy_from_slice(b"14000000000\0");
    h[148..156].copy_from_slice(b"        ");
    h[156] = kind;
    h[157..157 + link.len().min(100)].copy_from_slice(&link[..link.len().min(100)]);
    h[257..265].copy_from_slice(b"ustar\x0000");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut out = h.to_vec();
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out
}

fn tar_of(entries: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out: Vec<u8> = entries.concat();
    out.extend_from_slice(&[0u8; 1024]);
    out
}

/// Parent-directory snapshot guard: the destination is `<tmp>/work/dst`.
fn hostile_dst(tmp: &Tmp) -> PathBuf {
    let dst = tmp.path().join("work/dst");
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(tmp.path().join("work/neighbour"), b"n").unwrap();
    dst
}

fn outside(tmp: &Tmp) -> Vec<(PathBuf, String)> {
    snapshot(tmp.path())
        .into_iter()
        .filter(|(p, _)| !p.starts_with("work/dst") && p != Path::new("work/dst"))
        .filter(|(p, _)| {
            !p.to_string_lossy().ends_with(".zip") && !p.to_string_lossy().ends_with(".tar")
        })
        .collect()
}

#[test]
fn a_zip_slip_zip_writes_nothing_outside_the_destination() {
    let tmp = Tmp::new("r-slip-zip");
    let dst = hostile_dst(&tmp);
    let abs = tmp.path().join("abs-evil");
    let abs_name = abs.to_str().unwrap().as_bytes().to_vec();
    let zip = raw_zip(&[
        (b"../../evil", b"x"),
        (b"../work/dst/../../evil2", b"x"),
        (abs_name.as_slice(), b"x"),
        (b"a/../../b", b"x"),
        (b"..\\..\\win-evil", b"x"), // backslashes are ordinary name characters on Unix
        (b"good.txt", b"good"),
    ]);
    let archive = tmp.path().join("slip.zip");
    std::fs::write(&archive, zip).unwrap();
    if !backend_ready(&archive) {
        return;
    }
    let before = outside(&tmp);
    let out = extract(&archive, &[], &dst, ExtractOpts::default());
    assert!(out.is_success(), "{out:?}");
    assert_eq!(outside(&tmp), before, "something escaped the destination");
    assert_absent(&abs);
    assert_eq!(read(&dst.join("good.txt")), b"good");
    assert_no_part_files(tmp.path());
}

#[test]
fn a_tar_with_link_tricks_cannot_write_through_a_symlink() {
    let tmp = Tmp::new("r-slip-tar");
    let dst = hostile_dst(&tmp);
    let victim_dir = tmp.path().join("victim");
    std::fs::create_dir_all(&victim_dir).unwrap();
    std::fs::write(victim_dir.join("passwd"), b"original").unwrap();
    let tar = tar_of(vec![
        tar_entry(b"link", b'2', victim_dir.to_str().unwrap().as_bytes(), b""),
        tar_entry(b"link/passwd", b'0', b"", b"pwned"),
        tar_entry(b"rel", b'2', b"../../victim", b""),
        tar_entry(b"rel/passwd", b'0', b"", b"pwned2"),
        tar_entry(b"hl", b'1', b"/etc/passwd", b""),
        tar_entry(b"../outside.txt", b'0', b"", b"pwned3"),
        tar_entry(b"fifo", b'6', b"", b""),
        tar_entry(b"chr", b'3', b"", b""),
        tar_entry(b"ok.txt", b'0', b"", b"ok"),
    ]);
    let archive = tmp.path().join("links.tar");
    std::fs::write(&archive, tar).unwrap();
    if !backend_ready(&archive) {
        return;
    }
    let before = outside(&tmp);
    let out = extract(&archive, &[], &dst, ExtractOpts::default());
    assert!(out.is_success(), "{out:?}");
    assert_eq!(outside(&tmp), before);
    assert_eq!(read(&victim_dir.join("passwd")), b"original");
    assert_eq!(read(&dst.join("ok.txt")), b"ok");
    for gone in ["hl", "fifo", "chr"] {
        assert_absent(&dst.join(gone));
    }
    // The links themselves were refused (absolute / climbing target), so what follows them is just
    // a file in a real directory below the destination: never a write through a link.
    for dir in ["link", "rel"] {
        let md = std::fs::symlink_metadata(dst.join(dir)).unwrap();
        assert!(md.is_dir() && !md.file_type().is_symlink(), "{dir}");
    }
    assert_eq!(read(&dst.join("link/passwd")), b"pwned");
    assert_no_part_files(tmp.path());
}

#[test]
fn a_gzip_bomb_is_stopped_by_the_ratio_guard() {
    if !have("gzip") {
        return;
    }
    let tmp = Tmp::new("r-bomb");
    let dst = tmp.dst();
    let size = 32usize << 20; // 32 MiB of zeros: about 32 KB gzipped (about 1000:1)
    let tar = tar_of(vec![
        tar_entry(b"small.txt", b'0', b"", b"fine"),
        tar_entry(b"zeros.bin", b'0', b"", &vec![0u8; size]),
    ]);
    let plain = tmp.path().join("bomb.tar");
    std::fs::write(&plain, tar).unwrap();
    sh(tmp.path(), "gzip -9 -f bomb.tar");
    let archive = tmp.path().join("bomb.tar.gz");
    if !backend_ready(&archive) {
        return;
    }
    let opts = ExtractOpts {
        limits: ArchiveLimits {
            max_ratio: 100,
            ..ArchiveLimits::default()
        },
        ..ExtractOpts::default()
    };
    let out = extract(&archive, &[], &dst, opts);
    assert!(
        matches!(&out, Outcome::Failed { error, .. } if error.contains("bomb") || error.contains("limit")),
        "{out:?}"
    );
    assert_no_part_files(&dst);
    // Whatever was written is complete: no truncated zeros.bin under its final name.
    if let Ok(md) = std::fs::metadata(dst.join("zeros.bin")) {
        assert_eq!(md.len(), size as u64);
    }
}
