//! T3.6b (review finding m2): trashing items whose names are too long for `<name>.trashinfo`.
//! Uses an explicit private trash directory, never the real trash.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t32_support;

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_jobs::ops::trash::{TrashEnv, trash_path_with};
use ofm_jobs::{JobSpec, Outcome, run_sync};
use ofm_testkit::temp::TempDir;
use t32_support::{TestFs, no_questions, questions, report};

fn env_in(dir: &TempDir) -> TrashEnv {
    TrashEnv {
        home_trash: dir.path().join("share/Trash"),
        uid: rustix::process::getuid().as_raw(),
    }
}

fn names(dir: &Path) -> Vec<OsString> {
    let mut v: Vec<OsString> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().file_name())
        .collect();
    v.sort();
    v
}

fn percent(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Trash a file called `name`, then check the trash is consistent and restorable.
fn trash_one(t: &TempDir, env: &TrashEnv, name: &[u8]) -> (OsString, String) {
    let f: PathBuf = t.path().join(OsString::from_vec(name.to_vec()));
    fs::write(&f, b"payload").unwrap();
    let before = names(&env.home_trash.join("files"));
    trash_path_with(env, &f).unwrap_or_else(|e| panic!("{} bytes: {e:?}", name.len()));
    assert!(!f.exists());
    let after = names(&env.home_trash.join("files"));
    let stored: Vec<&OsString> = after.iter().filter(|n| !before.contains(n)).collect();
    assert_eq!(stored.len(), 1);
    let stored = stored[0].clone();
    assert!(stored.len() <= 245, "stored name {} bytes", stored.len());
    assert_eq!(
        fs::read(env.home_trash.join("files").join(&stored)).unwrap(),
        b"payload"
    );
    let mut info_name = stored.as_bytes().to_vec();
    info_name.extend_from_slice(b".trashinfo");
    let info = fs::read_to_string(
        env.home_trash
            .join("info")
            .join(OsString::from_vec(info_name)),
    )
    .unwrap();
    // `Path=` keeps the full original name so that restore works.
    let canon = fs::canonicalize(t.path()).unwrap();
    let mut full = canon.as_os_str().as_bytes().to_vec();
    full.push(b'/');
    full.extend_from_slice(name);
    assert_eq!(
        info.lines().nth(1).unwrap(),
        format!("Path={}", percent(&full))
    );
    assert_eq!(info.lines().count(), 3);
    (stored, info)
}

#[test]
fn name_of_255_bytes_is_trashed_with_shortened_stored_name() {
    let t = TempDir::new().unwrap();
    let env = env_in(&t);
    let mut name = vec![b'a'; 251];
    name.extend_from_slice(b".txt");
    assert_eq!(name.len(), 255);
    let (stored, _) = trash_one(&t, &env, &name);
    assert!(stored.as_bytes().ends_with(b".txt"), "extension is kept");
}

#[test]
fn name_of_250_bytes_is_trashed() {
    let t = TempDir::new().unwrap();
    let env = env_in(&t);
    trash_one(&t, &env, &[b'b'; 250]);
}

#[test]
fn names_around_the_limit_work() {
    let t = TempDir::new().unwrap();
    let env = env_in(&t);
    for len in [244usize, 245, 246] {
        let (stored, _) = trash_one(&t, &env, &vec![b'c'; len]);
        if len <= 245 {
            assert_eq!(stored.len(), len, "short enough names are stored unchanged");
        }
    }
}

#[test]
fn equal_long_prefixes_and_repeated_names_get_distinct_stored_names() {
    let t = TempDir::new().unwrap();
    let env = env_in(&t);
    let mut a = vec![b'x'; 254];
    a.push(b'1');
    let mut b = vec![b'x'; 254];
    b.push(b'2');
    let (sa, _) = trash_one(&t, &env, &a);
    let (sb, _) = trash_one(&t, &env, &b);
    let (sa2, _) = trash_one(&t, &env, &a); // same name trashed again
    assert!(sa != sb && sa != sa2 && sb != sa2);
    assert_eq!(names(&env.home_trash.join("files")).len(), 3);
    assert_eq!(names(&env.home_trash.join("info")).len(), 3);
}

#[test]
fn long_names_with_newline_and_non_utf8_bytes() {
    let t = TempDir::new().unwrap();
    let env = env_in(&t);
    let mut name = b"bad-\xff\n%".to_vec();
    name.extend(std::iter::repeat_n(b'z', 248));
    assert_eq!(name.len(), 255);
    let (_, info) = trash_one(&t, &env, &name);
    assert!(info.contains("bad-%FF%0A%25zzz"));
}

#[test]
fn delete_job_with_long_name_trashes_without_questions() {
    let t = TempDir::new().unwrap();
    let trash = TempDir::new().unwrap();
    let home = trash.path().join("Trash");
    let f = t.path().join("n".repeat(255));
    fs::write(&f, b"x").unwrap();
    let (sink, rec) = no_questions();
    let out = run_sync(
        &JobSpec::Delete {
            paths: vec![f.clone()],
            to_trash: true,
        },
        Arc::new(TestFs::with_trash(&home)),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!f.exists());
    assert_eq!(report(&out).files_done, 1);
    assert!(questions(&rec).is_empty(), "no permanent-delete offer");
    assert_eq!(names(&home.join("files")).len(), 1);
}
