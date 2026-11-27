//! A second `ofm DIR1 [DIR2]` while OFM runs: the running window opens the directories (first
//! in the active panel, second in the other one), relative to the caller's working directory,
//! with non-UTF-8 names intact and locked tabs left alone. Both processes talk over a private
//! session bus started by the test; the developer's real bus is never touched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod real_support;

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ofm_config::session::{Session, TabSession};
use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, have, tools_available};

/// A bus with no service directories: nothing of the host (portals, gvfs, keyring) can be
/// started from it.
const BUS_CONFIG: &str = r#"<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

/// A `dbus-daemon` of our own, killed when dropped.
struct PrivateBus {
    daemon: Child,
    address: OsString,
    /// Holds the config file and serves as the apps' `XDG_RUNTIME_DIR`.
    dir: ofm_testkit::temp::TempDir,
}

impl PrivateBus {
    fn start() -> PrivateBus {
        let dir = ofm_testkit::temp::TempDir::in_dir(&std::env::temp_dir()).unwrap();
        let config = dir.path().join("bus.conf");
        std::fs::write(&config, BUS_CONFIG).unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg("--config-file")
            .arg(&config)
            .args(["--nofork", "--print-address=1"])
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn dbus-daemon");
        let mut line = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = OsString::from(line.trim());
        assert!(!address.is_empty(), "dbus-daemon printed no address");
        PrivateBus {
            daemon,
            address,
            dir,
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// Run `ofm args...` from `cwd` as a second instance and wait for it to hand over and exit.
fn second_instance(first: &Running, bus: &PrivateBus, cwd: &Path, args: &[&OsStr]) {
    let home = first.data_home.parent().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ofm"))
        .args(args)
        .current_dir(cwd)
        .env("DISPLAY", &first.x.display)
        .env("GDK_BACKEND", "x11")
        .env("NO_AT_BRIDGE", "1")
        .env("GTK_A11Y", "none")
        .env("GSK_RENDERER", "cairo")
        .env_remove("WAYLAND_DISPLAY")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", first.paths.config_dir.parent().unwrap())
        .env("XDG_STATE_HOME", first.paths.state_dir.parent().unwrap())
        .env("XDG_DATA_HOME", &first.data_home)
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("XDG_RUNTIME_DIR", bus.dir.path())
        .spawn()
        .expect("spawn second ofm");
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "second instance failed: {status}");
            break;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the second instance did not exit (it opened its own window?)");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // The running instance gets the request over the bus and lists the folders on workers.
    std::thread::sleep(Duration::from_millis(1200));
}

fn path_of(tab: &TabSession) -> PathBuf {
    match (&tab.path, &tab.path_bytes) {
        (_, Some(bytes)) => PathBuf::from(OsString::from_vec(bytes.clone())),
        (Some(text), None) => PathBuf::from(text),
        (None, None) => PathBuf::new(),
    }
}

fn paths_of(session: &Session, side: usize) -> Vec<PathBuf> {
    session.panels[side].tabs.iter().map(path_of).collect()
}

#[test]
fn a_second_launch_opens_its_directories_in_the_running_window() {
    if !tools_available() || !have("dbus-daemon") {
        eprintln!("SKIP: Xvfb, xdotool or dbus-daemon missing");
        return;
    }
    let bus = PrivateBus::start();
    let left = TreeSpec::new().file("a.txt", FileSpec::content(b"a"));
    let right = TreeSpec::new().file("x1", FileSpec::content(b"x"));
    let running = Running::launch_env(
        "",
        &std::env::temp_dir(),
        &left,
        &right,
        &[
            ("DBUS_SESSION_BUS_ADDRESS", &bus.address),
            // The private bus has no accessibility registry; GTK would fail to register.
            ("GTK_A11Y", OsStr::new("none")),
            ("XDG_RUNTIME_DIR", bus.dir.path().as_os_str()),
        ],
    );
    let (start_left, start_right) = (running.left.clone(), running.right.clone());

    // The caller's working directory, with a plain and a non-UTF-8 folder in it.
    let caller = ofm_testkit::temp::TempDir::in_dir(&std::env::temp_dir()).unwrap();
    let base = caller.path().canonicalize().unwrap();
    let odd = OsStr::from_bytes(b"odd-\xff name");
    for dir in [
        OsStr::new("rel one"),
        odd,
        OsStr::new("third"),
        OsStr::new("4th"),
    ] {
        std::fs::create_dir(base.join(dir)).unwrap();
    }

    // Two relative directories: active (left) panel, then the other one.
    second_instance(
        &running,
        &bus,
        &base,
        &[OsStr::new("rel one"), OsStr::new("third")],
    );
    // One (non-UTF-8) directory while the right panel is active: only that panel moves.
    running.keys(&["Tab"]);
    second_instance(&running, &bus, &base, &[odd]);
    // No arguments: the window is only raised.
    second_instance(&running, &bus, &base, &[]);
    // A locked tab stays where it is; the directory opens in a new tab.
    running.keys(&["ctrl+shift+l"]);
    second_instance(&running, &bus, &base, &[OsStr::new("4th")]);

    let (session, _running) = running.quit();
    assert_eq!(
        paths_of(&session, 0),
        vec![base.join("rel one")],
        "left was {start_left:?}"
    );
    assert_eq!(
        paths_of(&session, 1),
        vec![base.join(odd), base.join("4th")],
        "right was {start_right:?}"
    );
    assert!(session.panels[1].tabs[0].locked);
    assert_eq!(session.panels[1].active_tab, 1);
    assert_eq!(session.active_panel, 1);
}
