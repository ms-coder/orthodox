//! User/group name tables for the properties dialog. Reading `/etc/passwd` and `/etc/group`
//! happens on a worker (through the `Fs` seam of `ofm-jobs`), the dialog only sees the result.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use ofm_jobs::{Fs, RealFs};
use ofm_vfs::pool::IoPool;

use crate::wiring;

/// Names and ids of users and groups.
#[derive(Debug, Clone, Default)]
pub struct IdNames {
    users: HashMap<String, u32>,
    groups: HashMap<String, u32>,
    user_ids: HashMap<u32, String>,
    group_ids: HashMap<u32, String>,
}

/// Parse `name:x:id:...` lines (passwd and group share this prefix).
fn parse(text: &str) -> Vec<(String, u32)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split(':');
            let (name, _, id) = (parts.next()?, parts.next()?, parts.next()?);
            Some((name.to_owned(), id.parse().ok()?))
        })
        .collect()
}

impl IdNames {
    /// Build from the text of `/etc/passwd` and `/etc/group`.
    pub fn from_texts(passwd: &str, group: &str) -> IdNames {
        let mut n = IdNames::default();
        for (name, id) in parse(passwd) {
            n.user_ids.entry(id).or_insert_with(|| name.clone());
            n.users.entry(name).or_insert(id);
        }
        for (name, id) in parse(group) {
            n.group_ids.entry(id).or_insert_with(|| name.clone());
            n.groups.entry(name).or_insert(id);
        }
        n
    }

    /// Name of user `uid`, or its number.
    pub fn user_name(&self, uid: u32) -> String {
        self.user_ids
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    }

    /// Name of group `gid`, or its number.
    pub fn group_name(&self, gid: u32) -> String {
        self.group_ids
            .get(&gid)
            .cloned()
            .unwrap_or_else(|| gid.to_string())
    }

    /// Resolve user text: a number, or a known name.
    pub fn user_id(&self, text: &str) -> Result<u32, String> {
        resolve(text, &self.users).ok_or_else(|| format!("unknown user '{text}'"))
    }

    /// Resolve group text: a number, or a known name.
    pub fn group_id(&self, text: &str) -> Result<u32, String> {
        resolve(text, &self.groups).ok_or_else(|| format!("unknown group '{text}'"))
    }
}

fn resolve(text: &str, table: &HashMap<String, u32>) -> Option<u32> {
    let text = text.trim();
    text.parse().ok().or_else(|| table.get(text).copied())
}

fn read_text(fs: &dyn Fs, path: &Path) -> String {
    let Ok(mut f) = fs.open_read_follow(path) else {
        return String::new();
    };
    let (mut bytes, mut buf) = (Vec::new(), vec![0u8; 64 * 1024]);
    while let Ok(n) = f.read(&mut buf) {
        if n == 0 || bytes.len() > 4 << 20 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Load the tables on the I/O pool; the receiver yields one [`IdNames`].
pub fn load(pool: &IoPool) -> async_channel::Receiver<Arc<IdNames>> {
    wiring::run_on_pool(pool, |tx| {
        let fs = RealFs;
        let names = IdNames::from_texts(
            &read_text(&fs, Path::new("/etc/passwd")),
            &read_text(&fs, Path::new("/etc/group")),
        );
        let _ = tx.send_blocking(Arc::new(names));
    })
}

/// True when the process runs with root privileges (chown is then allowed).
pub fn is_root() -> bool {
    RealFs.is_root()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids_roundtrip() {
        let n = IdNames::from_texts(
            "root:x:0:0:root:/root:/bin/sh\nbob:x:1000:1000::/h:/bin/sh\nbroken\n",
            "root:x:0:\nstaff:x:50:bob\n",
        );
        assert_eq!(n.user_name(1000), "bob");
        assert_eq!(n.user_name(4242), "4242");
        assert_eq!(n.group_name(50), "staff");
        assert_eq!(n.user_id("bob"), Ok(1000));
        assert_eq!(n.user_id(" 7 "), Ok(7));
        assert_eq!(n.group_id("staff"), Ok(50));
        assert!(n.user_id("nobody-here").is_err());
        assert!(n.group_id("bob").is_err(), "users are not groups");
    }
}
