//! Running one program (a terminal editor from F4) in its own VTE widget: the exact argument
//! vector is the child's argv, no shell line is typed anywhere, so quoting, a half-typed
//! shell line and shells without idle detection are all irrelevant.
//!
//! VTE's spawn API takes `&str` arguments only, but paths are bytes (CLAUDE.md invariant 4):
//! when every word is UTF-8 the vector is passed as it is; otherwise the child is
//! `/bin/sh -c <script> sh` where the script rebuilds each word byte for byte from octal
//! escapes (`printf` format strings) and `exec`s the real program. The same applies to the
//! working directory — a non-UTF-8 folder must not silently become "inherit ours" (a
//! `rm -i %f` there would hit a same-named file in OFM's own directory).

use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The argv to hand to `spawn_async` so that the child runs exactly `argv` in `cwd`
/// (bytes exact in both). When `cwd` is `None` or UTF-8 and every word is UTF-8, the words
/// pass through unchanged and the caller gives `cwd` to `spawn_async` as usual; otherwise
/// the child is `/bin/sh -c <script> sh` that `cd`s to the byte-exact directory (when it is
/// not UTF-8) and rebuilds every non-UTF-8 word from octal escapes before `exec`.
pub fn spawn_argv_in(argv: &[OsString], cwd: Option<&Path>) -> Vec<String> {
    let cwd_octal = cwd.filter(|c| c.to_str().is_none());
    if cwd_octal.is_none() {
        let direct: Option<Vec<String>> =
            argv.iter().map(|a| a.to_str().map(str::to_owned)).collect();
        if let Some(d) = direct {
            return d;
        }
    }
    let mut script = String::from("set --\n");
    if let Some(dir) = cwd_octal {
        script.push_str("v=$(printf '");
        for b in dir.as_os_str().as_bytes() {
            // Writing to a String cannot fail.
            let _ = write!(script, "\\{b:03o}");
        }
        // The trailing `x` keeps a final newline of the path from being stripped.
        script.push_str("'; printf x)\ncd -- \"${v%x}\" || exit 1\n");
    }
    for a in argv {
        script.push_str("v=$(printf '");
        for b in a.as_bytes() {
            let _ = write!(script, "\\{b:03o}");
        }
        script.push_str("'; printf x)\nset -- \"$@\" \"${v%x}\"\n");
    }
    script.push_str("exec \"$@\"\n");
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script,
        "sh".to_owned(),
    ]
}

/// The argv to hand to `spawn_async` so that the child runs exactly `argv`.
pub fn spawn_argv(argv: &[OsString]) -> Vec<String> {
    spawn_argv_in(argv, None)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::os::unix::ffi::OsStringExt;
    use std::process::Command;

    use super::*;

    fn run_printing_args(argv: &[OsString]) -> Vec<u8> {
        // `printf '%s\0'` as the program prints every received argument NUL-terminated.
        let plan = spawn_argv(argv);
        let out = Command::new(&plan[0]).args(&plan[1..]).output().unwrap();
        assert!(out.status.success(), "{plan:?}");
        out.stdout
    }

    #[test]
    fn utf8_argv_passes_through_unchanged() {
        let a = vec![OsString::from("vim"), OsString::from("/tmp/a b/é.txt")];
        assert_eq!(spawn_argv(&a), vec!["vim", "/tmp/a b/é.txt"]);
    }

    #[test]
    fn non_utf8_words_survive_byte_for_byte() {
        let nasty: Vec<u8> = b"/tmp/\xff\xfe it's \"q\" $HOME `x` %s \\n\n".to_vec();
        let a = vec![
            OsString::from("printf"),
            OsString::from("%s\\0"),
            OsString::from_vec(nasty.clone()),
            OsString::from("-x"),
        ];
        let mut want = nasty;
        want.push(0);
        want.extend_from_slice(b"-x\0");
        assert_eq!(run_printing_args(&a), want);
    }

    /// T8.10: a non-UTF-8 folder must be the child's cwd, byte for byte — previously the
    /// terminal fell back to "inherit ours", so `rm -i %f` in such a folder operated on a
    /// same-named file in OFM's own directory.
    #[test]
    fn non_utf8_cwd_is_byte_exact() {
        let base = ofm_testkit::temp::TempDir::new().unwrap();
        let dir = base
            .path()
            .join(std::ffi::OsString::from_vec(b"we ird\xff'\n$HOME".to_vec()));
        std::fs::create_dir(&dir).unwrap();
        let elsewhere = base.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let argv = [OsString::from("pwd")];
        let plan = spawn_argv_in(&argv, Some(&dir));
        // Not a plain pass-through: the cd is baked into the wrapper script.
        assert_eq!(plan[0], "/bin/sh");
        assert!(plan[2].contains("cd -- "));
        // The wrapper runs in OFM's directory but lands the child in the byte-exact folder.
        let out = Command::new(&plan[0])
            .args(&plan[1..])
            .current_dir(&elsewhere)
            .output()
            .unwrap();
        assert!(out.status.success(), "{plan:?}");
        // `pwd` prints the physical path plus its own newline (the folder name contains one
        // too — that byte is part of the path and must survive).
        let mut want = dir.canonicalize().unwrap().as_os_str().as_bytes().to_vec();
        want.push(b'\n');
        assert_eq!(out.stdout, want);
        // UTF-8 cwd: pass-through words, caller's job (spawn's own cwd argument).
        let plan = spawn_argv_in(&argv, Some(std::path::Path::new("/tmp")));
        assert_eq!(plan, vec!["pwd"]);
    }
}
