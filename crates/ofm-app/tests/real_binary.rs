//! Real-binary smoke tests: launch the `ofm` executable under its own Xvfb, send real X key events
//! with `xdotool`, quit with F10 and assert on the saved `session.toml` (plus screenshots for
//! sanity). Library-level tests cannot catch bugs in `app::run()` wiring (e.g. a dropped
//! `AppContext` that silently kills every key), so this is the only test that runs the real
//! start-up path.
//!
//! Key injection: `xdotool` finds and focuses the window, but its `key F5` on the stock Xvfb/evdev
//! keymap is sent as Alt+F5 (the F-keys are CTRL+ALT-type keys and xdotool 3.2016 picks the Alt
//! level), so keys go through a 30-line Python/ctypes helper that calls XTest directly (the same
//! mechanism xdotool uses, minus the keymap guesswork).
//!
//! Skips (with a log line) when `Xvfb`, `xdotool`, `import` (ImageMagick) or `python3` is missing.
//! Needs no outer display: it starts its own Xvfb. The app runs with temp XDG dirs only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::chunks_exact_to_as_chunks,
    clippy::disallowed_methods // test fixture setup runs on the test thread, not the GTK thread
)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ofm_config::ConfigPaths;
use ofm_config::session::{PanelSession, Session};
use ofm_testkit::tree::{FileSpec, TreeSpec};

fn have(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn tools_available() -> bool {
    let ok = ["Xvfb", "xdotool", "import", "python3"]
        .iter()
        .all(|t| have(t));
    if !ok {
        eprintln!("skipping: Xvfb/xdotool/import/python3 not installed");
    }
    ok
}

/// Presses a chord ("ctrl+t", "F10", "BackSpace") through XTest. Usage: `xkey.py CHORD...`.
const XKEY_PY: &str = r#"
import ctypes, sys, time
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
    time.sleep(0.05)
x.XSync(d, 0)
"#;

/// A private X server; killed on drop.
struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    fn start() -> Xvfb {
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
struct Shot {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Shot {
    fn distinct_colors(&self) -> usize {
        let mut set = std::collections::HashSet::new();
        for p in self.px.chunks_exact(3) {
            set.insert([p[0], p[1], p[2]]);
        }
        set.len()
    }
    fn diff_pixels(&self, other: &Shot) -> usize {
        assert_eq!((self.w, self.h), (other.w, other.h));
        self.px
            .chunks_exact(3)
            .zip(other.px.chunks_exact(3))
            .filter(|(a, b)| a != b)
            .count()
    }
}

/// A running `ofm` with its own X server, fixture directories and XDG dirs.
struct Running {
    x: Xvfb,
    app: Child,
    /// Keeps the temp dirs alive (their type is not nameable from here).
    _keep: Vec<Box<dyn std::any::Any>>,
    left: PathBuf,
    right: PathBuf,
    paths: ConfigPaths,
}

impl Running {
    fn x(&self, program: &str, args: &[&str]) -> std::process::Output {
        Command::new(program)
            .args(args)
            .env("DISPLAY", &self.x.display)
            .output()
            .unwrap_or_else(|e| panic!("{program}: {e}"))
    }

    /// Launch with optional `config.toml` text. Returns once the window is mapped, focused and
    /// the panels had time to load.
    fn launch(config: &str) -> Running {
        let x = Xvfb::start();
        let xdg = TreeSpec::new().build_temp().unwrap();
        let left = TreeSpec::new()
            .file("a.txt", FileSpec::content(b"a"))
            .file("b.txt", FileSpec::content(b"b"))
            .dir(
                "sub",
                TreeSpec::new().file("inner.txt", FileSpec::content(b"i")),
            )
            .build_temp()
            .unwrap();
        let right = TreeSpec::new()
            .file("x1", FileSpec::content(b"x"))
            .file("x2", FileSpec::content(b"x"))
            .file("x3", FileSpec::content(b"x"))
            .build_temp()
            .unwrap();
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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ofm");
        let mut r = Running {
            x,
            app,
            left: left.path().to_owned(),
            right: right.path().to_owned(),
            _keep: vec![Box::new(xdg), Box::new(left), Box::new(right)],
            paths,
        };
        let wid = r.wait_for_window();
        r.x("xdotool", &["windowfocus", &wid]);
        // Directory listings arrive from worker threads: wait for the first painted content.
        r.wait_for_content();
        r
    }

    fn wait_for_window(&mut self) -> String {
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

    fn wait_for_content(&self) {
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

    fn shot(&self) -> Shot {
        let out = self.x("import", &["-window", "root", "ppm:-"]);
        assert!(out.status.success(), "import failed");
        parse_ppm(&out.stdout)
    }

    fn save_png(&self, path: &Path) {
        let out = self.x("import", &["-window", "root", &path.to_string_lossy()]);
        assert!(out.status.success(), "import failed");
    }

    /// Send real key presses ("Tab", "F10", "ctrl+t") and give the app time to react after each.
    fn keys(&self, keys: &[&str]) {
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
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Quit with F10, wait for the process to exit and load the saved session.
    fn quit(mut self) -> (Session, Self) {
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

fn cursor(p: &PanelSession) -> Option<String> {
    p.tabs[p.active_tab].cursor_name.clone()
}

fn tab_path(p: &PanelSession, i: usize) -> PathBuf {
    p.tabs[i].path_buf().expect("tab path")
}

/// Tab switches the active panel, Down moves the cursor in that panel only, quit saves it.
#[test]
fn keys_tab_and_cursor_then_quit_saves_session() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    let before = s.shot();
    assert!(before.distinct_colors() >= 6, "window looks blank");
    s.keys(&["Tab", "Down", "Down"]);
    let after = s.shot();
    assert!(
        before.diff_pixels(&after) > 50,
        "screen did not change after Tab/Down/Down: keys are not reaching the app"
    );
    let (session, s) = s.quit();
    assert_eq!(session.active_panel, 1, "Tab did not switch panel");
    assert_eq!(session.panels.len(), 2);
    // Right dir sorted: `..`, x1, x2, x3 -> two Downs = x2.
    assert_eq!(cursor(&session.panels[1]).as_deref(), Some("x2"));
    let left_cursor = cursor(&session.panels[0]);
    assert!(
        matches!(left_cursor.as_deref(), None | Some("..")),
        "left cursor moved: {left_cursor:?}"
    );
    assert_eq!(tab_path(&session.panels[1], 0), s.right);
}

/// Navigation: Down onto `sub`, Enter descends; Backspace goes up and lands back on `sub`.
#[test]
fn keys_enter_dir_and_go_up() {
    if !tools_available() {
        return;
    }
    // Directories first: `..`, sub, a.txt, b.txt.
    let s = Running::launch("");
    s.keys(&["Down", "Return"]);
    let (session, s) = s.quit();
    assert_eq!(
        tab_path(&session.panels[0], 0),
        s.left.join("sub"),
        "Enter did not descend into the directory"
    );

    let s = Running::launch("");
    s.keys(&["Down", "Return", "BackSpace"]);
    let (session, s) = s.quit();
    assert_eq!(
        tab_path(&session.panels[0], 0),
        s.left,
        "Backspace did not go up"
    );
    assert_eq!(
        cursor(&session.panels[0]).as_deref(),
        Some("sub"),
        "cursor should return to the directory we came from"
    );
}

/// Tabs: Ctrl+T twice then Ctrl+W once leaves two tabs; view-mode keys stick.
#[test]
fn keys_tabs_and_view_mode() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    s.keys(&["ctrl+t", "ctrl+t", "ctrl+w", "ctrl+2"]);
    let (session, _s) = s.quit();
    let left = &session.panels[0];
    assert_eq!(left.tabs.len(), 2, "tab.new x2 + tab.close should leave 2");
    assert_eq!(left.active_tab, 1);
    assert_eq!(left.tabs[1].view, "full", "Ctrl+2 did not select Full view");

    let s = Running::launch("[panels]\ndefault_view = \"full\"\n");
    s.keys(&["ctrl+1"]);
    let (session, _s) = s.quit();
    assert_eq!(session.panels[0].tabs[0].view, "brief");
}

/// F9 opens the menu bar popover (screen changes), Escape closes it again, quit still works.
#[test]
fn keys_f9_opens_menu_and_escape_closes() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    let closed = s.shot();
    s.keys(&["F9"]);
    std::thread::sleep(Duration::from_millis(300));
    let open = s.shot();
    assert!(
        closed.diff_pixels(&open) > 500,
        "F9 did not visibly open a menu ({} px differ)",
        closed.diff_pixels(&open)
    );
    s.keys(&["Escape"]);
    std::thread::sleep(Duration::from_millis(300));
    let again = s.shot();
    assert!(
        open.diff_pixels(&again) > 500,
        "Escape did not close the menu"
    );
    let (session, _s) = s.quit();
    assert_eq!(session.panels.len(), 2, "quit after menu did not save");
}

/// Screenshot of the real binary (`target/screenshots/real-binary-smoke.png`), must not be blank.
#[test]
fn real_binary_screenshot_is_not_blank() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    assert!(s.shot().distinct_colors() >= 6);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join("real-binary-smoke.png"));
}
