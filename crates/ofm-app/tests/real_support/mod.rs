//! Shared harness for the real-binary tests (`real_binary.rs`, `real_binary_fileops.rs`): a private
//! Xvfb, the `ofm` executable with temp XDG dirs, XTest key injection and screenshots.
//! See `real_binary.rs` for the rationale behind each piece.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::chunks_exact_to_as_chunks,
    clippy::disallowed_methods
)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ofm_config::ConfigPaths;
use ofm_config::session::Session;
use ofm_testkit::tree::{FileSpec, TreeSpec};

fn log_sink() -> Stdio {
    match std::env::var_os("OFM_E2E_LOG") {
        Some(f) => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(f)
            .map_or_else(|_| Stdio::null(), Stdio::from),
        None => Stdio::null(),
    }
}

pub fn have(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn tools_available() -> bool {
    let ok = ["Xvfb", "xdotool", "import", "python3"]
        .iter()
        .all(|t| have(t));
    if !ok {
        eprintln!("skipping: Xvfb/xdotool/import/python3 not installed");
    }
    ok
}

/// Presses a chord ("ctrl+t", "F10", "BackSpace") through XTest. Usage: `xkey.py CHORD...`.
pub const XKEY_PY: &str = r#"
import ctypes, os, sys, time
x = ctypes.CDLL("libX11.so.6")
t = ctypes.CDLL("libXtst.so.6")
x.XOpenDisplay.restype = ctypes.c_void_p
x.XStringToKeysym.restype = ctypes.c_ulong
x.XKeysymToKeycode.restype = ctypes.c_ubyte
x.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
t.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
d = x.XOpenDisplay(None)
if not d:
    sys.exit("cannot open display")
MODS = {"ctrl": "Control_L", "alt": "Alt_L", "shift": "Shift_L"}
def code(name):
    c = x.XKeysymToKeycode(d, x.XStringToKeysym(MODS.get(name.lower(), name).encode()))
    if not c:
        sys.exit("unknown key " + name)
    return c
for chord in sys.argv[1:]:
    codes = [code(p) for p in chord.split("+")]
    for c in codes:
        t.XTestFakeKeyEvent(d, c, 1, 0)
    for c in reversed(codes):
        t.XTestFakeKeyEvent(d, c, 0, 0)
    x.XFlush(d)
    time.sleep(float(os.environ.get("XKEY_GAP", "0.05")))
x.XSync(d, 0)
"#;

/// A private X server; killed on drop.
pub struct Xvfb {
    pub child: Child,
    pub display: String,
}

impl Xvfb {
    pub fn start() -> Xvfb {
        // `-displayfd 1` makes Xvfb pick a free display number and print it to stdout.
        let mut child = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "1600x1000x24",
                "-nolisten",
                "tcp",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Xvfb");
        let out = child.stdout.take().expect("stdout");
        let mut line = String::new();
        BufReader::new(out)
            .read_line(&mut line)
            .expect("display number");
        Xvfb {
            child,
            display: format!(":{}", line.trim()),
        }
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Raw RGB screenshot.
pub struct Shot {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Shot {
    pub fn distinct_colors(&self) -> usize {
        let mut set = std::collections::HashSet::new();
        for p in self.px.chunks_exact(3) {
            set.insert([p[0], p[1], p[2]]);
        }
        set.len()
    }
    pub fn diff_pixels(&self, other: &Shot) -> usize {
        assert_eq!((self.w, self.h), (other.w, other.h));
        self.px
            .chunks_exact(3)
            .zip(other.px.chunks_exact(3))
            .filter(|(a, b)| a != b)
            .count()
    }
}

/// A running `ofm` with its own X server, fixture directories and XDG dirs.
pub struct Running {
    pub x: Xvfb,
    pub app: Child,
    /// Keeps the temp dirs alive (their type is not nameable from here).
    _keep: Vec<Box<dyn std::any::Any>>,
    pub left: PathBuf,
    pub right: PathBuf,
    pub paths: ConfigPaths,
    /// `$XDG_DATA_HOME` of the app (Trash lives under it).
    pub data_home: PathBuf,
}

impl Running {
    pub fn x(&self, program: &str, args: &[&str]) -> std::process::Output {
        Command::new(program)
            .args(args)
            .env("DISPLAY", &self.x.display)
            .output()
            .unwrap_or_else(|e| panic!("{program}: {e}"))
    }

    /// Launch with optional `config.toml` text. Returns once the window is mapped, focused and
    /// the panels had time to load.
    pub fn launch(config: &str) -> Running {
        let left = TreeSpec::new()
            .file("a.txt", FileSpec::content(b"a"))
            .file("b.txt", FileSpec::content(b"b"))
            .dir(
                "sub",
                TreeSpec::new().file("inner.txt", FileSpec::content(b"i")),
            );
        let right = TreeSpec::new()
            .file("x1", FileSpec::content(b"x"))
            .file("x2", FileSpec::content(b"x"))
            .file("x3", FileSpec::content(b"x"));
        Running::launch_with(config, &left, &right)
    }

    /// Launch with `config.toml` text and the given fixture trees for the left/right panel.
    pub fn launch_with(config: &str, left: &TreeSpec, right: &TreeSpec) -> Running {
        Running::launch_in(config, &std::env::temp_dir(), left, right)
    }

    /// Like [`Running::launch_with`], with the panel directories created under `base` (for
    /// tests that need a real disk instead of a tmpfs `/tmp`).
    pub fn launch_in(config: &str, base: &Path, left: &TreeSpec, right: &TreeSpec) -> Running {
        Running::launch_env(config, base, left, right, &[])
    }

    /// Like [`Running::launch_in`] with extra environment variables for the app (`EDITOR`, ...).
    pub fn launch_env(
        config: &str,
        base: &Path,
        left: &TreeSpec,
        right: &TreeSpec,
        env: &[(&str, &std::ffi::OsStr)],
    ) -> Running {
        let x = Xvfb::start();
        let xdg = TreeSpec::new().build_temp().unwrap();
        let mk = |spec: &TreeSpec| {
            let dir = ofm_testkit::temp::TempDir::in_dir(base).unwrap();
            spec.build(dir.path()).unwrap();
            dir
        };
        let left = mk(left);
        let right = mk(right);
        let xdg_path = xdg.path().to_owned();
        let config_home = xdg.path().join("config");
        let state_home = xdg.path().join("state");
        std::fs::create_dir_all(config_home.join("ofm")).unwrap();
        std::fs::create_dir_all(&state_home).unwrap();
        if !config.is_empty() {
            std::fs::write(config_home.join("ofm/config.toml"), config).unwrap();
        }
        // Same resolution as the binary: <XDG_*_HOME>/ofm.
        let paths = ConfigPaths::with_dirs(config_home.join("ofm"), state_home.join("ofm"));
        let app = Command::new(env!("CARGO_BIN_EXE_ofm"))
            .arg(left.path())
            .arg(right.path())
            .env("DISPLAY", &x.display)
            .env("GDK_BACKEND", "x11")
            .env("GSK_RENDERER", "cairo")
            .env("NO_AT_BRIDGE", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_STATE_HOME", &state_home)
            .env("XDG_DATA_HOME", xdg.path().join("data"))
            .env("HOME", xdg.path())
            // No session bus: GApplication is then not single-instance, so a developer's running
            // OFM (or a leftover one) can never swallow our launch.
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
            .envs(env.iter().copied())
            // Debugging aid: OFM_E2E_LOG=<file> captures the app's tracing output (stdout) and
            // GTK warnings (stderr); combine with RUST_LOG=debug.
            .stdout(log_sink())
            .stderr(log_sink())
            .spawn()
            .expect("spawn ofm");
        let mut r = Running {
            x,
            app,
            left: left.path().to_owned(),
            right: right.path().to_owned(),
            _keep: vec![Box::new(xdg), Box::new(left), Box::new(right)],
            paths,
            data_home: xdg_path.join("data"),
        };
        let wid = r.wait_for_window();
        r.x("xdotool", &["windowfocus", &wid]);
        // Directory listings arrive from worker threads: wait for the first painted content.
        r.wait_for_content();
        r
    }

    /// Start the app again after it exited (e.g. after [`Running::quit`]): same X server and XDG
    /// directories, the given directory arguments (none = restore the saved session). Returns
    /// once the window is mapped, focused and painted.
    pub fn restart(&mut self, args: &[&Path]) {
        assert!(
            self.app.try_wait().unwrap().is_some(),
            "restart needs the previous instance to have exited"
        );
        let config_home = self.paths.config_dir.parent().unwrap().to_owned();
        let state_home = self.paths.state_dir.parent().unwrap().to_owned();
        let home = self.data_home.parent().unwrap().to_owned();
        self.app = Command::new(env!("CARGO_BIN_EXE_ofm"))
            .args(args)
            .env("DISPLAY", &self.x.display)
            .env("GDK_BACKEND", "x11")
            .env("GSK_RENDERER", "cairo")
            .env("NO_AT_BRIDGE", "1")
            .env_remove("WAYLAND_DISPLAY")
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_STATE_HOME", &state_home)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("HOME", &home)
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
            .stdout(log_sink())
            .stderr(log_sink())
            .spawn()
            .expect("respawn ofm");
        let wid = self.wait_for_window();
        self.x("xdotool", &["windowfocus", &wid]);
        self.wait_for_content();
    }

    pub fn wait_for_window(&mut self) -> String {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            assert!(
                self.app.try_wait().unwrap().is_none(),
                "ofm exited during start-up"
            );
            let out = self.x("xdotool", &["search", "--onlyvisible", "--class", "ofm"]);
            let ids = String::from_utf8_lossy(&out.stdout).into_owned();
            if let Some(id) = ids.lines().next() {
                return id.trim().to_owned();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("no ofm window appeared");
    }

    pub fn wait_for_content(&self) {
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            if self.shot().distinct_colors() >= 6 {
                // Chrome and lists are painted; allow the async listing to land.
                std::thread::sleep(Duration::from_millis(700));
                return;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        panic!("window never painted any content");
    }

    pub fn shot(&self) -> Shot {
        let out = self.x("import", &["-window", "root", "ppm:-"]);
        assert!(out.status.success(), "import failed");
        parse_ppm(&out.stdout)
    }

    pub fn save_png(&self, path: &Path) {
        let out = self.x("import", &["-window", "root", &path.to_string_lossy()]);
        assert!(out.status.success(), "import failed");
    }

    /// Send real key presses ("Tab", "F10", "ctrl+t") and give the app time to react after each.
    pub fn keys(&self, keys: &[&str]) {
        self.keys_paced(keys, 250);
    }

    /// Several chords in ONE helper process (about 50 ms apart): for time-critical sequences.
    pub fn burst(&self, chords: &[&str]) {
        self.burst_paced(chords, 50);
    }

    /// Like [`Running::burst`] with `gap_ms` between the chords.
    pub fn burst_paced(&self, chords: &[&str], gap_ms: u64) {
        let out = Command::new("python3")
            .env("XKEY_GAP", format!("{}", gap_ms as f64 / 1000.0))
            .args(["-I", "-c", XKEY_PY])
            .args(chords)
            .env("DISPLAY", &self.x.display)
            .output()
            .expect("python3");
        assert!(
            out.status.success(),
            "key burst failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Like [`Running::keys`] with `settle_ms` of waiting after each key.
    pub fn keys_paced(&self, keys: &[&str], settle_ms: u64) {
        for k in keys {
            let out = Command::new("python3")
                .args(["-I", "-c", XKEY_PY, k])
                .env("DISPLAY", &self.x.display)
                .output()
                .expect("python3");
            assert!(
                out.status.success(),
                "key injection {k} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            std::thread::sleep(Duration::from_millis(settle_ms));
        }
    }

    /// Quit with F10, wait for the process to exit and load the saved session.
    pub fn quit(mut self) -> (Session, Self) {
        self.keys(&["F10"]);
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if self.app.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < end,
                "ofm did not exit after F10 (quit key dead?)"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            self.paths.session_file().exists(),
            "quit did not write session.toml"
        );
        let (session, warnings) = Session::load(&self.paths);
        assert!(warnings.is_empty(), "session warnings: {warnings:?}");
        (session, self)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.app.kill();
        let _ = self.app.wait();
    }
}

fn parse_ppm(data: &[u8]) -> Shot {
    // "P6\n<w> <h>\n255\n" then raw RGB.
    let mut pos = 0;
    let mut tokens = Vec::new();
    while tokens.len() < 4 {
        while data[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let start = pos;
        while !data[pos].is_ascii_whitespace() {
            pos += 1;
        }
        tokens.push(String::from_utf8_lossy(&data[start..pos]).into_owned());
    }
    pos += 1;
    assert_eq!(tokens[0], "P6");
    Shot {
        w: tokens[1].parse().unwrap(),
        h: tokens[2].parse().unwrap(),
        px: data[pos..].to_vec(),
    }
}
