//! uid/gid to name cache, read once from `/etc/passwd` and `/etc/group`.

use std::collections::HashMap;
use std::sync::OnceLock;

type Table = HashMap<u32, String>;

fn parse(text: &str) -> Table {
    let mut map = Table::new();
    for line in text.lines() {
        let mut parts = line.split(':');
        let (Some(name), Some(_), Some(id)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if let Ok(id) = id.parse::<u32>() {
            map.entry(id).or_insert_with(|| name.to_owned());
        }
    }
    map
}

fn load(path: &str) -> Table {
    // Reading /etc once on first use is cheap and happens on a worker thread (listing/stat).
    std::fs::read(path)
        .map(|b| parse(&String::from_utf8_lossy(&b)))
        .unwrap_or_default()
}

/// Name of user `uid`, or its decimal form when unknown.
pub fn user_name(uid: u32) -> String {
    static USERS: OnceLock<Table> = OnceLock::new();
    USERS
        .get_or_init(|| load("/etc/passwd"))
        .get(&uid)
        .cloned()
        .unwrap_or_else(|| uid.to_string())
}

/// Name of group `gid`, or its decimal form when unknown.
pub fn group_name(gid: u32) -> String {
    static GROUPS: OnceLock<Table> = OnceLock::new();
    GROUPS
        .get_or_init(|| load("/etc/group"))
        .get(&gid)
        .cloned()
        .unwrap_or_else(|| gid.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_falls_back() {
        let t = parse("root:x:0:0:r:/root:/bin/sh\nbad line\nbob:x:1000:1000::/h:/bin/sh\n");
        assert_eq!(t.get(&0).map(String::as_str), Some("root"));
        assert_eq!(t.get(&1000).map(String::as_str), Some("bob"));
        assert_eq!(user_name(u32::MAX - 1), (u32::MAX - 1).to_string());
    }
}
