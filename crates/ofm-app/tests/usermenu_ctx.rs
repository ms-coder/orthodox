//! T8.2b: the user menu (F28) driven through `AppContext::execute` in-process (needs a display;
//! skips without one): listing, hotkey data, running an entry that writes a marker, prompts,
//! confirmation, failures, unsafe names, and the trust flow of a local `.ofm-menu.toml`.
//! One `#[test]` owns the GTK thread. `OFM_SHOT_DIR=dir` writes screenshots.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod fileops_support;

use std::path::Path;
use std::time::Duration;

use fileops_support::*;
use gtk::prelude::*;
use ofm_app::usermenu::{self, UserMenuState};
use ofm_core::cmd;
use ofm_testkit::tree::{FileSpec, TreeSpec};

const GLOBAL_MENU: &str = r#"
version = 1
[[item]]
key = "M"
title = "Marker"
command = "printf %%s %f > marker.out"
[[item]]
key = "P"
title = "Prompted"
command = "printf %%s %{input:Word} > prompt.out"
[[item]]
key = "C"
title = "Confirmed"
command = "printf yes > confirm.out"
confirm = true
[[item]]
key = "F"
title = "Fails"
command = "echo broken >&2; exit 4"
"#;

const LOCAL_MENU: &str = r#"
version = 1
[[item]]
key = "L"
title = "Local entry"
command = "printf local > local.out"
"#;

fn state(fx: &Fixture) -> std::rc::Rc<UserMenuState> {
    usermenu::state(&fx.ctx)
}

fn open_menu(fx: &Fixture) -> gtk::ListBox {
    fx.run(cmd::MENU_USER);
    let st = state(fx);
    pump_until("menu dialog", || st.open_dialog().is_some());
    fx.find("ofm-usermenu-list").downcast().unwrap()
}

fn close_menu(fx: &Fixture) {
    let st = state(fx);
    fx.run(cmd::DIALOG_CANCEL);
    pump_until("menu closed", || !st.busy());
}

fn select(list: &gtk::ListBox, index: i32) {
    list.select_row(list.row_at_index(index).as_ref());
}

fn wait_file(path: &Path) -> String {
    pump_until(&format!("{} written", path.display()), || path.exists());
    pump_for(100);
    std::fs::read_to_string(path).unwrap()
}

fn any_label_contains(root: &gtk::Widget, needle: &str) -> bool {
    if let Some(l) = root.downcast_ref::<gtk::Label>()
        && l.text().contains(needle)
    {
        return true;
    }
    let mut c = root.first_child();
    while let Some(w) = c {
        if any_label_contains(&w, needle) {
            return true;
        }
        c = w.next_sibling();
    }
    false
}

#[test]
fn user_menu_lists_runs_prompts_confirms_and_trusts() {
    if !init() {
        return;
    }
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .file("z\u{1b}x", FileSpec::content(b"bad name"));
    let right = TreeSpec::new().file("x1", FileSpec::content(b"x"));
    let fx = Fixture::new(
        "io.github.mscoder.OfmUserMenuTest",
        left,
        right,
        Some("far-blue"),
    );
    let menu_file = fx._cfg.path().join("config/menu.toml");
    std::fs::write(&menu_file, GLOBAL_MENU).unwrap();
    let dir = fx.left.path().to_owned();
    // `..`, a.txt, z<ESC>x: put the cursor on a.txt.
    fx.run(cmd::PANEL_END);
    fx.run(cmd::PANEL_CURSOR_UP);

    // ---- listing --------------------------------------------------------------------------
    let list = open_menu(&fx);
    assert_eq!(list_len(&list), 4);
    assert!(fx.try_find("ofm-usermenu-problems").is_none());
    fx.shot("usermenu-menu");
    close_menu(&fx);

    // ---- run an entry (Enter = dialog.ok) -> marker in the panel directory ----------------
    let list = open_menu(&fx);
    select(&list, 0);
    fx.run(cmd::DIALOG_OK);
    assert_eq!(wait_file(&dir.join("marker.out")), "a.txt");

    // ---- %{input:..}: Esc cancels the whole run, OK fills the answer ------------------------
    let list = open_menu(&fx);
    select(&list, 1);
    fx.run(cmd::DIALOG_OK);
    pump_until("input dialog", || {
        fx.try_find("ofm-usermenu-input")
            .is_some_and(|w| w.is_mapped())
    });
    fx.run(cmd::DIALOG_CANCEL);
    pump_for(400);
    assert!(
        !dir.join("prompt.out").exists(),
        "cancelled prompt ran anyway"
    );
    let list = open_menu(&fx);
    select(&list, 1);
    fx.run(cmd::DIALOG_OK);
    pump_until("input dialog again", || {
        fx.try_find("ofm-usermenu-input")
            .is_some_and(|w| w.is_mapped())
    });
    fx.entry("ofm-usermenu-input").set_text("hello");
    fx.run(cmd::DIALOG_OK);
    assert_eq!(wait_file(&dir.join("prompt.out")), "hello");

    // ---- confirm: the final command is shown; Esc does not run it ---------------------------
    let list = open_menu(&fx);
    select(&list, 2);
    fx.run(cmd::DIALOG_OK);
    pump_until("confirm dialog", || {
        fx.try_find("ofm-usermenu-command")
            .is_some_and(|w| w.is_mapped())
    });
    let shown = fx
        .find("ofm-usermenu-command")
        .downcast::<gtk::Label>()
        .unwrap();
    assert_eq!(shown.text(), "printf yes > confirm.out");
    fx.shot("usermenu-confirm");
    fx.run(cmd::DIALOG_CANCEL);
    pump_for(400);
    assert!(!dir.join("confirm.out").exists());
    let list = open_menu(&fx);
    select(&list, 2);
    fx.run(cmd::DIALOG_OK);
    pump_until("confirm dialog again", || {
        fx.try_find("ofm-usermenu-command")
            .is_some_and(|w| w.is_mapped())
    });
    fx.run(cmd::DIALOG_OK); // Enter on the focused Run button
    assert_eq!(wait_file(&dir.join("confirm.out")), "yes");

    // ---- a failing command shows the exit status and the stderr tail -----------------------
    let list = open_menu(&fx);
    select(&list, 3);
    fx.run(cmd::DIALOG_OK);
    pump_until("failure message", || {
        any_label_contains(fx.win.window.upcast_ref(), "exit status 4")
    });
    assert!(any_label_contains(fx.win.window.upcast_ref(), "broken"));
    fx.run(cmd::DIALOG_OK);
    pump_for(300);

    // ---- a name with control bytes is refused with a message, nothing runs ------------------
    std::fs::remove_file(dir.join("marker.out")).unwrap();
    fx.run(cmd::PANEL_END); // z<ESC>x
    let list = open_menu(&fx);
    select(&list, 0);
    fx.run(cmd::DIALOG_OK);
    pump_until("unsafe name message", || {
        any_label_contains(fx.win.window.upcast_ref(), "control bytes")
    });
    pump_for(300);
    assert!(!dir.join("marker.out").exists());
    fx.run(cmd::DIALOG_OK);
    pump_for(300);
    fx.run(cmd::PANEL_CURSOR_UP);

    // ---- problems row: a broken entry shows "N problems in menu files" ----------------------
    std::fs::write(
        &menu_file,
        format!("{GLOBAL_MENU}\n[[item]]\ntitle = \"bad\"\ncommand = \"echo %q\"\n"),
    )
    .unwrap();
    let _ = open_menu(&fx);
    let toggle = fx.button("ofm-usermenu-problems");
    assert!(toggle.label().unwrap().contains("problem"));
    toggle.emit_clicked();
    pump_for(400);
    fx.shot("usermenu-problems");
    close_menu(&fx);
    std::fs::write(&menu_file, GLOBAL_MENU).unwrap();

    // ---- trust flow of a local menu --------------------------------------------------------
    let local_file = dir.join(".ofm-menu.toml");
    std::fs::write(&local_file, LOCAL_MENU).unwrap();
    fx.run(cmd::MENU_USER);
    pump_until("trust prompt", || {
        fx.try_find("ofm-usermenu-trust-trust")
            .is_some_and(|w| w.is_mapped())
    });
    assert!(
        state(&fx).open_dialog().is_none(),
        "menu shown before the trust decision"
    );
    let hash = fx
        .find("ofm-usermenu-trust-hash")
        .downcast::<gtk::Label>()
        .unwrap()
        .text();
    assert!(hash.starts_with("sha256 ") && hash.len() == "sha256 ".len() + 64);
    fx.button("ofm-usermenu-trust-show").emit_clicked();
    pump_for(500);
    fx.shot("usermenu-trust");
    // Ignore: the menu opens without the local entry, offering a review.
    fx.button("ofm-usermenu-trust-ignore").emit_clicked();
    let st = state(&fx);
    // (a closing dialog stays in the widget tree for its animation: wait until it is gone, or
    // the next lookup could find the old buttons)
    pump_until("menu after ignore", || {
        st.open_dialog().is_some() && fx.try_find("ofm-usermenu-trust-trust").is_none()
    });
    let list: gtk::ListBox = fx.find("ofm-usermenu-list").downcast().unwrap();
    assert_eq!(list_len(&list), 4);
    assert!(fx.try_find("ofm-usermenu-ignored").is_some());
    assert!(!dir.join("local.out").exists());
    // Review -> Trust: now the local entry is first and runs.
    fx.button("ofm-usermenu-review").emit_clicked();
    pump_until("trust prompt again", || {
        st.open_dialog().is_none()
            && fx
                .try_find("ofm-usermenu-trust-trust")
                .is_some_and(|w| w.is_mapped())
    });
    fx.button("ofm-usermenu-trust-trust").emit_clicked();
    pump_until("menu after trust", || st.open_dialog().is_some());
    let list: gtk::ListBox = fx.find("ofm-usermenu-list").downcast().unwrap();
    assert_eq!(list_len(&list), 5);
    select(&list, 0);
    fx.run(cmd::DIALOG_OK);
    assert_eq!(wait_file(&dir.join("local.out")), "local");
    // F2 again: trusted, no prompt.
    let list = open_menu(&fx);
    assert_eq!(list_len(&list), 5);
    fx.shot("usermenu-with-local");
    close_menu(&fx);
    // Changed content: untrusted again, prompt before the menu.
    std::fs::write(&local_file, format!("{LOCAL_MENU}# edited\n")).unwrap();
    fx.run(cmd::MENU_USER);
    pump_until("trust prompt after change", || {
        fx.try_find("ofm-usermenu-trust-trust")
            .is_some_and(|w| w.is_mapped())
    });
    assert!(state(&fx).open_dialog().is_none());
    fx.run(cmd::DIALOG_CANCEL); // Esc = Ignore
    pump_until("menu after esc", || st.open_dialog().is_some());
    assert_eq!(
        list_len(
            &fx.find("ofm-usermenu-list")
                .downcast::<gtk::ListBox>()
                .unwrap()
        ),
        4
    );
    close_menu(&fx);
    std::thread::sleep(Duration::from_millis(10));
}
