//! POSIX shell quoting on bytes and the exact `cd` command OFM feeds to the shell (ADR-0003).

use std::ffi::OsStr;
use std::path::Path;

use super::bytes_of;

/// Quote `bytes` in POSIX single-quote form: wrapped in `'…'`, each `'` becomes `'\''`.
/// Any byte passes through unchanged inside the quotes, so non-UTF-8 names survive exactly
/// (round-trips through every POSIX shell; see the proptests for the reference un-quoter).
pub fn shell_quote_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    out.push(b'\'');
    for &b in bytes {
        if b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    out
}

/// [`shell_quote_bytes`] as a `String` for widgets that hold UTF-8 text (the command line).
///
/// When the quoted form would not be valid UTF-8 (a non-UTF-8 name), the result switches to
/// ANSI-C quoting `$'…'`, escaping every non-ASCII byte as `\xNN` (plus `'`, `\` and control
/// characters). `bash`, `zsh` and `ksh` expand that back to the original bytes; shells without
/// ANSI-C quoting (e.g. `fish`) may not — the byte-exact path for those is [`cd_command`],
/// which panel→shell directory sync always uses.
pub fn shell_quote(name: &OsStr) -> String {
    let quoted = shell_quote_bytes(&bytes_of(name));
    if let Ok(s) = String::from_utf8(quoted) {
        return s;
    }
    let bytes = bytes_of(name);
    let mut out = String::with_capacity(bytes.len() + 8);
    out.push_str("$'");
    for &b in bytes.iter() {
        match b {
            b'\'' => out.push_str("\\'"),
            b'\\' => out.push_str("\\\\"),
            0x00..=0x1f | 0x7f..=0xff => {
                out.push_str(&format!("\\x{b:02x}"));
            }
            _ => {
                // ASCII printable (0x20..=0x7e minus the two handled above).
                out.push(b as char);
            }
        }
    }
    out.push('\'');
    out
}

/// The exact bytes OFM feeds to an idle shell to move it to `path`:
/// `" cd -- '<quoted>'\n"` (ADR-0003). The leading space keeps the line out of the shell
/// history (`HISTCONTROL=ignorespace` is set by the injected rc snippet); `--` protects
/// against paths that look like options; the quoting is byte-exact via [`shell_quote_bytes`].
pub fn cd_command(path: &Path) -> Vec<u8> {
    let mut out = Vec::with_capacity(path.as_os_str().len() + 16);
    out.extend_from_slice(b" cd -- ");
    out.extend_from_slice(&shell_quote_bytes(&bytes_of(path.as_os_str())));
    out.push(b'\n');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::*;

    #[cfg(unix)]
    fn os(bytes: &[u8]) -> std::ffi::OsString {
        std::os::unix::ffi::OsStringExt::from_vec(bytes.to_vec())
    }

    #[test]
    fn quote_basics() {
        assert_eq!(shell_quote_bytes(b"plain"), b"'plain'");
        assert_eq!(shell_quote_bytes(b"it's"), b"'it'\\''s'");
        assert_eq!(shell_quote_bytes(b""), b"''");
        assert_eq!(shell_quote_bytes(b"a b\tc"), b"'a b\tc'");
        assert_eq!(shell_quote(OsStr::new("it's")), "'it'\\''s'");
    }

    #[test]
    #[cfg(unix)]
    fn quote_non_utf8_bytes_exact() {
        let name = os(&[0xff, b'a', 0xfe]);
        assert_eq!(shell_quote_bytes(&os_to_bytes(&name)), b"'\xffa\xfe'");
        // The String form falls back to ANSI-C quoting and round-trips byte-exactly in bash/zsh.
        assert_eq!(shell_quote(&name), "$'\\xffa\\xfe'");
    }

    #[cfg(unix)]
    fn os_to_bytes(s: &OsStr) -> Vec<u8> {
        std::os::unix::ffi::OsStrExt::as_bytes(s).to_vec()
    }

    #[test]
    #[cfg(unix)]
    fn quote_ansi_c_escapes() {
        // Control bytes are valid UTF-8 and stay in the single-quote form (POSIX-safe);
        // only non-UTF-8 bytes switch to ANSI-C quoting (see quote_non_utf8_bytes_exact).
        let name = os(b"nul\0nl\n");
        assert_eq!(shell_quote(&name), "'nul\0nl\n'");
    }

    #[test]
    fn cd_command_format() {
        let cmd = cd_command(Path::new("/tmp/my dir"));
        assert_eq!(cmd, b" cd -- '/tmp/my dir'\n");
        let cmd = cd_command(Path::new("/x'"));
        assert_eq!(cmd, b" cd -- '/x'\\'''\n");
    }
}
