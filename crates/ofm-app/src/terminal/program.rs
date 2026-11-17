//! Running one program (a terminal editor from F4) in its own VTE widget: the exact argument
//! vector is the child's argv, no shell line is typed anywhere, so quoting, a half-typed
//! shell line and shells without idle detection are all irrelevant.
//!
//! VTE's spawn API takes `&str` arguments only, but paths are bytes (CLAUDE.md invariant 4):
//! when every word is UTF-8 the vector is passed as it is; otherwise the child is
//! `/bin/sh -c <script> sh` where the script rebuilds each word byte for byte from octal
//! escapes (`printf` format strings) and `exec`s the real program.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt;

/// The argv to hand to `spawn_async` so that the child runs exactly `argv`.
pub fn spawn_argv(argv: &[OsString]) -> Vec<String> {
    let direct: Option<Vec<String>> = argv.iter().map(|a| a.to_str().map(str::to_owned)).collect();
    if let Some(d) = direct {
        return d;
    }
    let mut script = String::from("set --\n");
    for a in argv {
        script.push_str("v=$(printf '");
        for b in a.as_bytes() {
            // Writing to a String cannot fail.
            let _ = write!(script, "\\{b:03o}");
        }
        // The trailing `x` keeps a final newline of the word from being stripped.
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
        let a = vec![OsString::from("vim"), OsString::from("/tmp/a b/\u{e9}.txt")];
        assert_eq!(spawn_argv(&a), vec!["vim", "/tmp/a b/\u{e9}.txt"]);
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
}
