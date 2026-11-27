//! T7.1 integration tests (F24): the engine against generated fixture trees.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ofm_core::{CancelToken, EntryKind};
use ofm_search::{
    ContentMode, ContentQuery, Hit, NameFilter, NameMode, SearchEncoding, SearchError, SearchEvent,
    SearchHandle, SearchProgress, SearchQuery, SearchSummary, SearchWarning,
};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{DirSpec, FileSpec, TreeSpec, raw_name};
use ofm_viewer::encoding::Encoding;

/// Everything one search reported.
#[derive(Default)]
struct Run {
    hits: Vec<Hit>,
    warnings: Vec<SearchWarning>,
    progress: Vec<SearchProgress>,
    summary: Option<SearchSummary>,
    failure: Option<SearchError>,
}

impl Run {
    fn names(&self) -> Vec<String> {
        self.hits
            .iter()
            .map(|h| h.entry.name.to_string_lossy().into_owned())
            .collect()
    }
    fn hit(&self, name: &str) -> &Hit {
        self.hits
            .iter()
            .find(|h| h.entry.name.to_string_lossy() == name)
            .unwrap_or_else(|| panic!("no hit named {name}, have {:?}", self.names()))
    }
}

fn run(query: SearchQuery) -> Run {
    let handle = SearchHandle::start(query, CancelToken::new()).expect("search starts");
    let mut r = Run::default();
    while let Some(ev) = next_event(handle.events()) {
        match ev {
            SearchEvent::Found(batch) => r.hits.extend(batch),
            SearchEvent::Progress(p) => r.progress.push(p),
            SearchEvent::Warning(w) => r.warnings.push(w),
            SearchEvent::Done(s) => {
                r.summary = Some(s);
                break;
            }
            SearchEvent::Failed(e) => {
                r.failure = Some(e);
                break;
            }
        }
    }
    assert!(
        r.failure.is_none(),
        "search failed: {:?}",
        r.failure.as_ref().unwrap()
    );
    r
}

/// Drain one event with a 30 s watchdog (a broken engine must not hang the suite).
fn next_event(rx: &async_channel::Receiver<SearchEvent>) -> Option<SearchEvent> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match rx.try_recv() {
            Ok(ev) => return Some(ev),
            Err(async_channel::TryRecvError::Empty) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(async_channel::TryRecvError::Closed) => return None,
        }
    }
}

fn query_for(root: &Path) -> SearchQuery {
    SearchQuery {
        roots: vec![root.to_owned()],
        ..SearchQuery::default()
    }
}

fn with_name(query: &SearchQuery, pattern: &str) -> SearchQuery {
    let mut q = query.clone();
    q.name = NameFilter {
        pattern: pattern.into(),
        ..NameFilter::default()
    };
    q
}

fn basic_tree() -> TempDir {
    TreeSpec::new()
        .file("alpha.txt", FileSpec::content(b"one\n"))
        .file("beta.log", FileSpec::content(b"two\n"))
        .file("gamma.TXT", FileSpec::content(b"three\n"))
        .dir(
            "sub",
            DirSpec::new(TreeSpec::new().file("delta.txt", FileSpec::content(b"four\n"))),
        )
        .build_temp()
        .unwrap()
}

// ---------------------------------------------------------------- name filters

#[test]
fn name_glob_include_exclude_case() {
    let dir = basic_tree();
    let all = run(with_name(&query_for(dir.path()), ""));
    assert_eq!(
        all.names(),
        ["alpha.txt", "beta.log", "gamma.TXT", "sub", "delta.txt"]
    );

    let txt = run(with_name(&query_for(dir.path()), "*.txt"));
    // Case-insensitive by default: gamma.TXT matches too.
    assert_eq!(txt.names(), ["alpha.txt", "gamma.TXT", "delta.txt"]);

    let mut q = query_for(dir.path());
    q.name = NameFilter {
        pattern: "*.txt".into(),
        case_sensitive: true,
        ..NameFilter::default()
    };
    let cs = run(q);
    assert_eq!(cs.names(), ["alpha.txt", "delta.txt"]); // gamma.TXT stays out

    let excl = run(with_name(&query_for(dir.path()), "*;!*.log"));
    assert_eq!(excl.names(), ["alpha.txt", "gamma.TXT", "sub", "delta.txt"]);
}

#[test]
fn name_regex_mode() {
    let dir = basic_tree();
    let mut q = query_for(dir.path());
    q.name = NameFilter {
        pattern: r"^(alpha|beta)\.".into(),
        mode: NameMode::Regex,
        case_sensitive: true,
    };
    assert_eq!(run(q).names(), ["alpha.txt", "beta.log"]);

    let mut q = query_for(dir.path());
    q.name = NameFilter {
        pattern: r"[aeiou]{2}".into(), // "aa" or similar double vowels
        mode: NameMode::Regex,
        case_sensitive: true,
    };
    assert!(run(q).names().is_empty());
}

#[test]
fn size_filter() {
    let dir = TreeSpec::new()
        .file("small", FileSpec::size(10))
        .file("medium", FileSpec::size(100))
        .file("large", FileSpec::size(1000))
        .build_temp()
        .unwrap();
    let with = |mut q: SearchQuery| {
        q.match_dirs = false;
        run(q)
    };
    let mut q = query_for(dir.path());
    q.size.min = Some(50);
    assert_eq!(with(q).names(), ["large", "medium"]);

    let mut q = query_for(dir.path());
    q.size.max = Some(50);
    assert_eq!(with(q).names(), ["small"]);

    let mut q = query_for(dir.path());
    q.size.min = Some(20);
    q.size.max = Some(500);
    assert_eq!(with(q).names(), ["medium"]);

    let mut q = query_for(dir.path());
    q.size.min = Some(2000);
    assert_eq!(with(q).names().len(), 0);
}

#[test]
fn mtime_filter() {
    let base = ofm_testkit::tree::DEFAULT_MTIME_NS;
    let dir = TreeSpec::new()
        .file("old", FileSpec::content(b"x").mtime_ns(base))
        .file(
            "new",
            FileSpec::content(b"x").mtime_ns(base + 20_000_000_000),
        )
        .build_temp()
        .unwrap();
    let mut q = query_for(dir.path());
    q.mtime.after_ns = Some(base + 10_000_000_000);
    q.match_dirs = false;
    assert_eq!(run(q).names(), ["new"]);

    let mut q = query_for(dir.path());
    q.mtime.before_ns = Some(base + 10_000_000_000);
    q.match_dirs = false;
    assert_eq!(run(q).names(), ["old"]);

    let mut q = query_for(dir.path());
    q.mtime.after_ns = Some(base + 30_000_000_000);
    q.match_dirs = false;
    assert!(run(q).names().is_empty());
}

#[test]
fn match_dirs_flag() {
    let dir = basic_tree();
    let mut q = with_name(&query_for(dir.path()), "s*");
    q.match_dirs = false;
    assert_eq!(run(q).names().len(), 0);

    // A directory that fails the name filter is still descended into.
    let mut q = with_name(&query_for(dir.path()), "*.txt");
    q.match_dirs = true;
    assert_eq!(run(q).names(), ["alpha.txt", "gamma.TXT", "delta.txt"]);
}

#[test]
fn dirs_never_match_with_a_content_query() {
    let dir = basic_tree();
    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery::text("four"));
    q.match_dirs = true; // must be ignored
    let r = run(q);
    assert_eq!(r.names(), ["delta.txt"]);
    assert!(r.hit("delta.txt").content.is_some());
}

// ------------------------------------------------------------- traversal rules

#[test]
fn hidden_files() {
    let dir = TreeSpec::new()
        .file(".hidden", FileSpec::content(b"x"))
        .file("visible", FileSpec::content(b"x"))
        .dir(
            ".hdir",
            DirSpec::new(TreeSpec::new().file("inside", FileSpec::content(b"x"))),
        )
        .build_temp()
        .unwrap();
    let r = run(query_for(dir.path()));
    // DFS pre-order, entries sorted by name bytes (".hdir" < ".hidden").
    assert_eq!(r.names(), [".hdir", "inside", ".hidden", "visible"]);

    let mut q = query_for(dir.path());
    q.include_hidden = false;
    assert_eq!(run(q).names(), ["visible"]);
}

#[test]
fn max_depth_semantics() {
    let dir = TreeSpec::new()
        .file("top", FileSpec::content(b"x"))
        .dir(
            "d1",
            DirSpec::new(TreeSpec::new().file("mid", FileSpec::content(b"x")).dir(
                "d2",
                DirSpec::new(TreeSpec::new().file("deep", FileSpec::content(b"x"))),
            )),
        )
        .build_temp()
        .unwrap();
    let mut q = query_for(dir.path());
    q.max_depth = Some(0); // direct children only
    assert_eq!(run(q.clone()).names(), ["d1", "top"]);

    q.max_depth = Some(1);
    assert_eq!(run(q.clone()).names(), ["d1", "d2", "mid", "top"]);

    q.max_depth = None;
    assert_eq!(run(q).names(), ["d1", "d2", "deep", "mid", "top"]);
}

#[test]
fn multiple_roots_and_a_file_root() {
    let dir = basic_tree();
    let q = SearchQuery {
        roots: vec![dir.path().join("sub"), dir.path().join("alpha.txt")],
        ..SearchQuery::default()
    };
    let r = run(q);
    assert_eq!(r.names(), ["delta.txt", "alpha.txt"]); // roots searched in order
    assert_eq!(
        r.hit("alpha.txt").real_path(),
        Some(dir.path().join("alpha.txt").as_path())
    );
}

#[test]
fn symlinks_not_followed_by_default() {
    let dir = TreeSpec::new()
        .dir(
            "real",
            DirSpec::new(TreeSpec::new().file("inner.txt", FileSpec::content(b"x"))),
        )
        .symlink("link", "real")
        .symlink("broken", "nowhere")
        .build_temp()
        .unwrap();
    let r = run(query_for(dir.path()));
    // `link` is reported but not descended; `broken` is reported with a None target.
    assert_eq!(r.names(), ["broken", "link", "real", "inner.txt"]);
    assert_eq!(
        r.hit("broken").entry.kind,
        EntryKind::Symlink { target: None }
    );
    assert_eq!(
        r.hit("link").entry.kind,
        EntryKind::Symlink {
            target: Some(ofm_core::TargetKind::Dir)
        }
    );
    assert!(r.warnings.is_empty());
}

#[test]
fn follow_symlinks_descends_and_dedups() {
    let dir = TreeSpec::new()
        .dir(
            "real",
            DirSpec::new(TreeSpec::new().file("inner.txt", FileSpec::content(b"x"))),
        )
        .symlink("link", "real")
        .build_temp()
        .unwrap();
    let mut q = query_for(dir.path());
    q.follow_symlinks = true;
    let r = run(q);
    // `link` descends into the same (dev, ino): reported once, not scanned twice.
    assert_eq!(r.names(), ["link", "inner.txt", "real"]);
    assert_eq!(
        r.hits
            .iter()
            .filter(|h| h.entry.name == "inner.txt")
            .count(),
        1
    );
    assert!(r.warnings.is_empty());
}

#[test]
fn symlink_loop_warns_and_terminates() {
    let dir = TreeSpec::new()
        .dir(
            "d",
            DirSpec::new(TreeSpec::new().file("f", FileSpec::content(b"x"))),
        )
        .build_temp()
        .unwrap();
    std::os::unix::fs::symlink(".", dir.path().join("d/self")).unwrap();
    std::os::unix::fs::symlink("d", dir.path().join("d/loop")).unwrap();
    let mut q = query_for(dir.path());
    q.follow_symlinks = true;
    let r = run(q);
    assert_eq!(r.names(), ["d", "f", "loop", "self"]); // each entry once
    assert!(
        r.warnings
            .iter()
            .any(|w| w.message.contains("symlink loop")),
        "warnings: {:?}",
        r.warnings
    );
}

#[test]
fn same_filesystem_skips_other_dev() {
    let Some(pair) = ofm_testkit::crossdev::two_filesystems() else {
        eprintln!("no second filesystem available; skipping");
        return;
    };
    TreeSpec::new()
        .file("here", FileSpec::content(b"x"))
        .build(pair.first.path())
        .unwrap();
    TreeSpec::new()
        .file("there", FileSpec::content(b"x"))
        .build(pair.second.path())
        .unwrap();
    std::os::unix::fs::symlink(pair.second.path(), pair.first.path().join("other-fs")).unwrap();

    let mut q = query_for(pair.first.path());
    q.follow_symlinks = true;
    q.same_filesystem = false;
    assert_eq!(run(q).names(), ["here", "other-fs", "there"]);

    let mut q = query_for(pair.first.path());
    q.follow_symlinks = true;
    q.same_filesystem = true;
    assert_eq!(run(q).names(), ["here", "other-fs"]);
}

#[test]
fn unreadable_dir_is_a_warning_not_an_error() {
    if !locked_dirs_work() {
        eprintln!("running as root: permission tests are meaningless; skipping");
        return;
    }
    let dir = TreeSpec::new()
        .dir(
            "locked",
            DirSpec::new(TreeSpec::new().file("secret", FileSpec::content(b"x"))),
        )
        .file("open", FileSpec::content(b"x"))
        .build_temp()
        .unwrap();
    let locked = dir.path().join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("chmod 0 did not lock the dir (root?); skipping");
        return;
    }
    let r = run(query_for(dir.path()));
    assert!(r.names().contains(&"open".to_owned()));
    assert!(r.names().contains(&"locked".to_owned())); // the dir itself is listed
    assert!(!r.names().contains(&"secret".to_owned()));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.path == locked && w.message.contains("cannot read directory")),
        "warnings: {:?}",
        r.warnings
    );
    assert!(r.summary.is_some());
}

#[test]
fn unreadable_root_is_a_root_error() {
    if !locked_dirs_work() {
        eprintln!("running as root: skipping");
        return;
    }
    let dir = TreeSpec::new()
        .dir("locked", DirSpec::new(TreeSpec::new()))
        .build_temp()
        .unwrap();
    let locked = dir.path().join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let err = SearchHandle::start(query_for(&locked), CancelToken::new());
    assert!(matches!(err, Err(SearchError::Root { .. })));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Whether chmod 000 actually blocks read_dir for us (false as root).
fn locked_dirs_work() -> bool {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("l");
    std::fs::create_dir(&p).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    let blocked = std::fs::read_dir(&p).is_err();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    blocked
}

// --------------------------------------------------------------- content search

#[test]
fn content_text_match_details() {
    let dir = TreeSpec::new()
        .file(
            "poem",
            FileSpec::content(b"roses are red\nviolets are blue\nNeedle in line three\n"),
        )
        .file("empty", FileSpec::content(b""))
        .build_temp()
        .unwrap();
    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery::text("needle"));
    let r = run(q);
    assert_eq!(r.names(), ["poem"]);
    let ch = r.hit("poem").content.as_ref().unwrap();
    assert_eq!(ch.first.line, 3);
    assert_eq!(ch.first.byte_offset, 31); // 14 + 17 bytes of lines 1-2
    assert_eq!(ch.first.snippet, "Needle in line three");
    assert_eq!(&ch.first.snippet[ch.first.snippet_match.clone()], "Needle");
    assert_eq!(ch.count, 1);
}

#[test]
fn content_case_regex_wholewords() {
    let dir = TreeSpec::new()
        .file(
            "text",
            FileSpec::content(b"Cat catalog bobcat cat.\nCAT.\n"),
        )
        .build_temp()
        .unwrap();
    // Case-insensitive text finds all three lines' cats but not the word-embedded ones.
    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery::text("cat"));
    let r = run(q);
    assert_eq!(
        r.hit("text").content.as_ref().unwrap().count,
        5 // Cat, catalog, bobcat, cat., CAT.
    );

    // Whole words only.
    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery {
        whole_words: true,
        ..ContentQuery::text("cat")
    });
    let r = run(q);
    let ch = r.hit("text").content.as_ref().unwrap();
    assert_eq!(ch.count, 3); // Cat, cat., CAT.
    assert_eq!(ch.first.line, 1);

    // Case-sensitive regex: catalog, bobcat and cat. (not Cat, not CAT.).
    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery {
        pattern: r"ca(t|se)".into(),
        mode: ContentMode::Regex,
        case_sensitive: true,
        ..ContentQuery::text("cat")
    });
    let r = run(q);
    assert_eq!(r.hit("text").content.as_ref().unwrap().count, 3);
}

#[test]
fn content_encoding_utf16_and_fixed_single_byte() {
    let dir = tempfile::tempdir().unwrap();
    let mut utf16 = vec![0xFF, 0xFE];
    utf16.extend(
        "first line\nsecond has Nadel\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes()),
    );
    std::fs::write(dir.path().join("u16"), utf16).unwrap();
    let (cp1251, _, _) = encoding_rs::WINDOWS_1251.encode("текст с иголкой внутри\n");
    std::fs::write(dir.path().join("cp1251"), cp1251.as_ref()).unwrap();

    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery::text("Nadel"));
    let r = run(q);
    let ch = r.hit("u16").content.as_ref().unwrap();
    assert_eq!(ch.first.line, 2);
    // BOM(2) + "first line\n"(22) + "second has "(22).
    assert_eq!(ch.first.byte_offset, 2 + 11 * 2 + 11 * 2);
    assert_eq!(ch.first.snippet, "second has Nadel");

    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery {
        pattern: "иголкой".into(),
        encoding: SearchEncoding::Fixed(Encoding::Windows1251),
        ..ContentQuery::text("x")
    });
    let r = run(q);
    let ch = r.hit("cp1251").content.as_ref().unwrap();
    assert_eq!(ch.first.byte_offset, 8); // "текст с " is 8 single-byte chars
    assert!(ch.first.snippet.contains("иголкой"));
}

#[test]
fn content_binary_skip_and_override() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bin"), b"header\x00\x00needle\x00tail\n").unwrap();
    std::fs::write(dir.path().join("txt"), b"plain needle\n").unwrap();

    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery::text("needle"));
    assert_eq!(run(q).names(), ["txt"]);

    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery {
        search_binary: true,
        ..ContentQuery::text("needle")
    });
    let r = run(q);
    assert_eq!(r.names().len(), 2);
    assert!(r.hit("bin").content.is_some());
}

#[test]
fn content_match_straddling_the_256k_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let mut data = vec![b'x'; 256 * 1024 - 3];
    data.extend_from_slice(b"needle");
    data.extend_from_slice(b"\nrest\n");
    std::fs::write(dir.path().join("big"), &data).unwrap();
    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery::text("needle"));
    let r = run(q);
    let ch = r.hit("big").content.as_ref().unwrap();
    assert_eq!(ch.first.byte_offset, (256 * 1024 - 3) as u64);
    assert_eq!(ch.first.line, 1);
}

#[test]
fn content_huge_single_line_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut data = vec![b' '; 3 * 1024 * 1024];
    let at = 2_500_000;
    data[at..at + 6].copy_from_slice(b"needle");
    std::fs::write(dir.path().join("long"), &data).unwrap();
    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery::text("needle"));
    let r = run(q);
    let ch = r.hit("long").content.as_ref().unwrap();
    assert_eq!(ch.first.byte_offset, at as u64);
    assert_eq!(ch.first.line, 1);
    assert_eq!(ch.count, 1, "overlap re-scan must not double-count");
}

#[test]
fn content_unreadable_file_is_a_warning() {
    if !locked_files_work() {
        eprintln!("running as root: skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("secret");
    std::fs::write(&p, b"needle\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery::text("needle"));
    let r = run(q);
    assert!(r.hits.is_empty());
    assert!(
        r.warnings
            .iter()
            .any(|w| w.path == p && w.message.contains("cannot")),
        "warnings: {:?}",
        r.warnings
    );
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
}

/// Whether chmod 000 actually blocks file reads for us (false as root).
fn locked_files_work() -> bool {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("f");
    std::fs::write(&p, b"x").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    let blocked = std::fs::read(&p).is_err();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    blocked
}

// ------------------------------------------------------------------ non-UTF-8

#[test]
fn non_utf8_names_and_paths_everywhere() {
    let name = raw_name(b"caf\xe9.txt");
    let dir = TreeSpec::new()
        .file(name.clone(), FileSpec::content(b"needle\n"))
        .dir(
            raw_name(b"dir-\xff"),
            DirSpec::new(TreeSpec::new().file("ok", FileSpec::content(b"x"))),
        )
        .build_temp()
        .unwrap();

    let r = run(with_name(&query_for(dir.path()), "caf*"));
    assert_eq!(r.names().len(), 1);
    let hit = &r.hits[0];
    assert_eq!(hit.entry.name, name);
    let expected = dir.path().join(&name);
    assert_eq!(hit.real_path(), Some(expected.as_path()));
    assert_eq!(hit.location, ofm_core::Location::Local(expected.clone()));

    // Content search finds and reports the same byte path.
    let mut q = query_for(dir.path());
    q.content = Some(ContentQuery::text("needle"));
    let r = run(q);
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].real_path(), Some(expected.as_path()));

    // A non-UTF-8 directory is traversed.
    let r = run(with_name(&query_for(dir.path()), "ok"));
    assert_eq!(r.names(), ["ok"]);
}

// -------------------------------------------------------------- result plumbing

#[test]
fn batching_splits_large_results() {
    let spec = TreeSpec::new();
    let spec = (0..700).fold(spec, |s, i| {
        s.file(format!("f{i:03}.txt"), FileSpec::content(b"x"))
    });
    let dir = spec.build_temp().unwrap();
    let handle = SearchHandle::start(
        with_name(&query_for(dir.path()), "*.txt"),
        CancelToken::new(),
    )
    .unwrap();
    let mut batches = Vec::new();
    let mut summary = None;
    while let Some(ev) = next_event(handle.events()) {
        match ev {
            SearchEvent::Found(b) => batches.push(b.len()),
            SearchEvent::Done(s) => {
                summary = Some(s);
                break;
            }
            SearchEvent::Failed(e) => panic!("failed: {e}"),
            _ => {}
        }
    }
    assert_eq!(batches.iter().sum::<usize>(), 700);
    assert!(batches.iter().all(|n| *n <= ofm_search::BATCH_SIZE));
    assert!(batches.len() > 1, "700 hits must not arrive in one batch");
    assert_eq!(summary.unwrap().hits, 700);
}

#[test]
fn progress_events_report_counters() {
    let dir = basic_tree();
    let r = run(query_for(dir.path()));
    assert!(!r.progress.is_empty(), "at least one Progress expected");
    let last = r.progress.last().unwrap();
    assert!(last.dirs_scanned >= 2); // root + sub
    assert!(last.files_scanned >= 4);
    assert_eq!(last.hits as usize, r.hits.len());
}

#[test]
fn max_hits_truncates() {
    let spec = (0..50).fold(TreeSpec::new(), |s, i| {
        s.file(format!("n{i:02}"), FileSpec::content(b"x"))
    });
    let dir = spec.build_temp().unwrap();
    let mut q = query_for(dir.path());
    q.max_hits = Some(10);
    let r = run(q);
    assert_eq!(r.hits.len(), 10);
    let s = r.summary.unwrap();
    assert!(s.truncated);
    assert!(!s.cancelled);
    assert_eq!(s.hits, 10);
}

#[test]
fn cancellation_stops_within_200ms_on_a_large_tree() {
    let dir = tempfile::tempdir().unwrap();
    for d in 0..20 {
        let sub = dir.path().join(format!("d{d:02}"));
        std::fs::create_dir(&sub).unwrap();
        for f in 0..500 {
            std::fs::write(sub.join(format!("f{f:03}")), b"").unwrap();
        }
    }
    let token = CancelToken::new();
    let handle = SearchHandle::start(query_for(dir.path()), token.clone()).unwrap();
    // Wait until the search is visibly working, then cancel.
    let mut got_any = false;
    while let Some(ev) = next_event(handle.events()) {
        match ev {
            SearchEvent::Progress(_) | SearchEvent::Found(_) => {
                got_any = true;
                break;
            }
            SearchEvent::Done(_) | SearchEvent::Failed(_) => break,
            SearchEvent::Warning(_) => {}
        }
    }
    assert!(got_any, "no progress event before finishing?");
    let t0 = Instant::now();
    token.cancel();
    let mut cancelled = false;
    while let Some(ev) = next_event(handle.events()) {
        match ev {
            SearchEvent::Done(s) => {
                cancelled = s.cancelled;
                break;
            }
            SearchEvent::Failed(e) => panic!("failed: {e}"),
            _ => {}
        }
    }
    let dt = t0.elapsed();
    assert!(cancelled, "summary must say cancelled");
    assert!(
        dt < Duration::from_millis(200),
        "cancel took {:?}, want < 200 ms",
        dt
    );
}

#[test]
fn dropping_the_receiver_stops_the_search() {
    let dir = basic_tree();
    let token = CancelToken::new();
    let handle = SearchHandle::start(query_for(dir.path()), token.clone()).unwrap();
    drop(handle); // cancels via Drop
    std::thread::sleep(Duration::from_millis(100));
    assert!(token.is_cancelled());
}

// ------------------------------------------------------------------- archives

#[test]
fn corrupt_archives_warn_for_each_archive() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.zip", "b.zip", "c.zip"] {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PK\x03\x04");
        bytes.extend_from_slice(&[0u8; 64]);
        std::fs::write(dir.path().join(name), &bytes).unwrap();
    }
    std::fs::write(dir.path().join("plain.txt"), b"x").unwrap();
    let mut q = query_for(dir.path());
    q.search_archives = true;
    let r = run(q);
    // The archives themselves are still name hits, and the plain file still works. With real
    // backends, each malformed archive gets its own corruption warning.
    for name in ["a.zip", "b.zip", "c.zip"] {
        assert!(r.names().contains(&name.to_owned()));
        assert!(
            r.warnings.iter().any(|w| {
                w.path == dir.path().join(name) && w.message.contains("corrupt archive")
            }),
            "missing corruption warning for {name}: {:?}",
            r.warnings
        );
    }
    assert!(r.names().contains(&"plain.txt".to_owned()));
    assert_eq!(r.warnings.len(), 3, "warnings: {:?}", r.warnings);
}

#[test]
fn archive_entries_are_filtered_not_the_outer_archive_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("inside.txt"), b"inside\n").unwrap();
    let archive = dir.path().join("data.tar");
    let ok = std::process::Command::new("tar")
        .arg("-cf")
        .arg(&archive)
        .arg("-C")
        .arg(dir.path())
        .arg("inside.txt")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("system tar unavailable; skipping");
        return;
    }
    let mut q = with_name(&query_for(dir.path()), "*.txt");
    q.search_archives = true;
    let r = run(q);
    assert!(r.hits.iter().any(|h| matches!(
        &h.location,
        ofm_core::Location::Archive { archive: a, inner }
            if a == &archive && inner == Path::new("inside.txt")
    )));
    assert!(!r.names().contains(&"data.tar".to_owned()));
}

#[test]
fn archive_content_search() {
    // Build a real tar with the system tool; requires the T6.1 backends.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("inside.txt"), b"needle in archive\n").unwrap();
    let archive = dir.path().join("data.tar");
    let ok = std::process::Command::new("tar")
        .arg("-cf")
        .arg(&archive)
        .arg("-C")
        .arg(dir.path())
        .arg("inside.txt")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("system tar unavailable; skipping even though ignored");
        return;
    }
    let mut q = query_for(dir.path());
    q.search_archives = true;
    q.content = Some(ContentQuery::text("needle"));
    let r = run(q);
    let hit = r
        .hits
        .iter()
        .find(|h| matches!(h.location, ofm_core::Location::Archive { .. }))
        .expect("archive hit");
    assert!(hit.real_path().is_none());
    assert!(hit.content.is_some());
}

/// `docs/` (a directory), `docs/small.txt`, `docs/big.txt`, all dated 2023-01-01, written to
/// `data.7z`; `secret.txt` is added encrypted when asked.
fn sevenz_fixture(encrypted_entry: bool) -> tempfile::TempDir {
    use sevenz_rust2::encoder_options::{AesEncoderOptions, EncoderOptions};
    use sevenz_rust2::{
        ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod, NtTime, Password,
    };
    let entry = |name: &str, is_directory: bool| ArchiveEntry {
        name: name.to_owned(),
        is_directory,
        has_stream: !is_directory,
        has_last_modified_date: true,
        last_modified_date: NtTime::new(133_170_048_000_000_000),
        ..ArchiveEntry::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let mut writer = ArchiveWriter::create(dir.path().join("data.7z")).unwrap();
    writer.set_encrypt_header(false);
    writer
        .push_archive_entry(entry("docs", true), None::<&[u8]>)
        .unwrap();
    writer
        .push_archive_entry(entry("docs/small.txt", false), Some(&b"needle"[..]))
        .unwrap();
    let big = b"needle ".repeat(100);
    writer
        .push_archive_entry(entry("docs/big.txt", false), Some(&big[..]))
        .unwrap();
    if encrypted_entry {
        writer.set_content_methods(vec![
            EncoderConfiguration::new(EncoderMethod::AES256_SHA256).with_options(
                EncoderOptions::Aes(AesEncoderOptions::new(Password::from("secret"))),
            ),
            EncoderConfiguration::new(EncoderMethod::LZMA2),
        ]);
        writer
            .push_archive_entry(entry("secret.txt", false), Some(&b"needle"[..]))
            .unwrap();
    }
    writer.finish().unwrap();
    dir
}

/// Inner paths of the archive hits, sorted.
fn inner_hits(r: &Run) -> Vec<String> {
    let mut names: Vec<String> = r
        .hits
        .iter()
        .filter_map(|h| match &h.location {
            ofm_core::Location::Archive { inner, .. } => Some(inner.display().to_string()),
            _ => None,
        })
        .collect();
    names.sort();
    names
}

#[test]
fn directories_in_archives_follow_the_directory_rules() {
    let dir = sevenz_fixture(false);
    let mut q = query_for(dir.path());
    q.search_archives = true;
    let r = run(q.clone());
    assert!(inner_hits(&r).contains(&"docs".to_owned()));

    q.match_dirs = false;
    let r = run(q.clone());
    assert_eq!(inner_hits(&r), ["docs/big.txt", "docs/small.txt"]);

    q.match_dirs = true;
    q.content = Some(ContentQuery::text("needle"));
    let r = run(q);
    assert_eq!(inner_hits(&r), ["docs/big.txt", "docs/small.txt"]);
}

#[test]
fn size_and_mtime_filters_apply_inside_archives() {
    let dir = sevenz_fixture(false);
    let mut q = with_name(&query_for(dir.path()), "*.txt");
    q.search_archives = true;
    q.size.min = Some(100);
    assert_eq!(inner_hits(&run(q.clone())), ["docs/big.txt"]);

    q.size.min = None;
    let new_year_2023 = 1_672_531_200 * 1_000_000_000;
    q.mtime.after_ns = Some(new_year_2023 - 1);
    q.mtime.before_ns = Some(new_year_2023 + 1);
    assert_eq!(
        inner_hits(&run(q.clone())),
        ["docs/big.txt", "docs/small.txt"]
    );

    q.mtime.after_ns = Some(new_year_2023 + 1);
    q.mtime.before_ns = None;
    assert!(inner_hits(&run(q)).is_empty());
}

#[test]
fn encrypted_entries_do_not_hide_the_plain_ones_from_content_search() {
    let dir = sevenz_fixture(true);
    let mut q = query_for(dir.path());
    q.search_archives = true;
    q.content = Some(ContentQuery::text("needle"));
    let r = run(q);
    assert_eq!(inner_hits(&r), ["docs/big.txt", "docs/small.txt"]);
    assert_eq!(r.warnings.len(), 1, "warnings: {:?}", r.warnings);
    assert!(r.warnings[0].message.contains("1 encrypted entries"));
}

#[test]
fn plain_files_outside_the_filter_are_not_content_hits_when_probing_archives() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"needle\n").unwrap();
    std::fs::write(dir.path().join("b.log"), b"needle\n").unwrap();
    let mut q = with_name(&query_for(dir.path()), "*.txt");
    q.search_archives = true;
    q.content = Some(ContentQuery::text("needle"));
    assert_eq!(run(q).names(), ["a.txt"]);
}

#[test]
fn unreadable_files_outside_the_filter_are_silent_when_probing_archives() {
    if !locked_files_work() {
        eprintln!("running as root: skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for name in ["locked.txt", "locked.bin"] {
        let p = dir.path().join(name);
        std::fs::write(&p, b"needle\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    for content in [None, Some(ContentQuery::text("needle"))] {
        let mut q = with_name(&query_for(dir.path()), "*.txt");
        q.search_archives = true;
        q.content = content;
        let r = run(q);
        let warned: Vec<&Path> = r.warnings.iter().map(|w| w.path.as_path()).collect();
        assert_eq!(warned, [dir.path().join("locked.txt")]);
    }
}

// ------------------------------------------------------------------- perf (opt-in)

#[test]
#[ignore = "perf: ~100k files"]
fn perf_name_search_over_100k_files_under_2s() {
    let dir = tempfile::tempdir().unwrap();
    for d in 0..200 {
        let sub = dir.path().join(format!("d{d:03}"));
        std::fs::create_dir(&sub).unwrap();
        for f in 0..500 {
            std::fs::write(sub.join(format!("file{f:03}.txt")), b"").unwrap();
        }
    }
    let t0 = Instant::now();
    let r = run(with_name(&query_for(dir.path()), "*.txt"));
    let dt = t0.elapsed();
    assert_eq!(r.hits.len(), 100_000);
    assert!(dt < Duration::from_secs(2), "name search took {dt:?}");
}

#[test]
#[ignore = "perf: writes ~1 GB"]
fn perf_content_search_streams_first_hit_under_500ms() {
    let dir = tempfile::tempdir().unwrap();
    let line = format!("{}\n", "filler".repeat(100)); // ~607 bytes/line
    for i in 0..64 {
        let mut data = Vec::with_capacity(16 * 1024 * 1024);
        if i == 0 {
            data.extend_from_slice(b"aardvark needle at the very start\n");
        }
        while data.len() < 16 * 1024 * 1024 {
            data.extend_from_slice(line.as_bytes());
        }
        std::fs::write(dir.path().join(format!("blob{i:02}")), &data).unwrap();
    }
    let t0 = Instant::now();
    let handle = SearchHandle::start(
        {
            let mut q = query_for(dir.path());
            q.content = Some(ContentQuery::text("needle"));
            q
        },
        CancelToken::new(),
    )
    .unwrap();
    let mut first_hit_at = None;
    while let Some(ev) = next_event(handle.events()) {
        match ev {
            SearchEvent::Found(b) if !b.is_empty() => {
                first_hit_at = Some(t0.elapsed());
                handle.cancel();
                break;
            }
            SearchEvent::Done(_) | SearchEvent::Failed(_) => break,
            _ => {}
        }
    }
    let dt = first_hit_at.expect("no hit streamed");
    assert!(dt < Duration::from_millis(500), "first hit took {dt:?}");
}

// ------------------------------------------------------------------- misc

#[test]
fn search_root_must_exist() {
    let err = SearchHandle::start(
        SearchQuery {
            roots: vec![PathBuf::from("/definitely/not/here/ofm-t71")],
            ..SearchQuery::default()
        },
        CancelToken::new(),
    );
    assert!(matches!(err, Err(SearchError::Root { .. })));
}
