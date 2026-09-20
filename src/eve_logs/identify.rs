//! Map EVE log files to character names via the `Listener:` header.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Max age for a log file to still count as "this session".
pub const MAX_LOG_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Read the `Listener: Name` header from a log file. Chatlogs are
/// UTF-16 LE; gamelogs UTF-8. Returns None if the file can't be read
/// or has no Listener line in the first 8 KiB.
pub fn extract_listener(path: &Path, is_chatlog: bool) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let header = if bytes.len() > 8192 {
        &bytes[..8192]
    } else {
        &bytes
    };
    let text = if is_chatlog {
        decode_utf16_prefix(header)
    } else {
        String::from_utf8_lossy(header).into_owned()
    };
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{FEFF}');
        if let Some(name) = line.strip_prefix("Listener:") {
            let name = name.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn decode_utf16_prefix(bytes: &[u8]) -> String {
    let usable = bytes.len() - (bytes.len() % 2);
    if usable == 0 {
        return String::new();
    }
    let u16s: Vec<u16> = bytes[..usable]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let mut s = String::from_utf16_lossy(&u16s);
    if s.starts_with('\u{FEFF}') {
        s.remove(0);
    }
    s
}

pub fn is_chatlog_filename(name: &str) -> bool {
    name.len() >= 6 && name[..6].eq_ignore_ascii_case("Local_") && name.ends_with(".txt")
}

/// Newest log file per character in `dir`. `chatlogs = true` restricts
/// to `Local_*.txt`; otherwise every `*.txt`. Files older than
/// `MAX_LOG_AGE` are skipped. First (newest mtime) wins per character
/// (compared case-insensitively).
pub fn newest_logs_by_character(dir: &Path, chatlogs: bool) -> HashMap<String, PathBuf> {
    let mut out: HashMap<String, PathBuf> = HashMap::new();
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return out,
    };
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, PathBuf, bool)> = Vec::new();
    for ent in rd.flatten() {
        let path = ent.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.ends_with(".txt") {
            continue;
        }
        let is_chat = is_chatlog_filename(name);
        if chatlogs && !is_chat {
            continue;
        }
        let meta = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if let Ok(age) = now.duration_since(mtime) {
            if age > MAX_LOG_AGE {
                continue;
            }
        }
        files.push((mtime, path, is_chat));
    }
    files.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, path, is_chat) in files {
        let Some(listener) = extract_listener(&path, is_chat) else {
            continue;
        };
        let key = listener.to_lowercase();
        out.entry(key).or_insert(path);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "nicotine-identify-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_utf16(path: &Path, text: &str) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xFEFFu16.to_le_bytes());
        for u in text.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        fs::write(path, bytes).unwrap();
    }

    fn write_utf8(path: &Path, text: &str) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn listener_from_chatlog() {
        let dir = temp_dir();
        let path = dir.join("Local_20240115_120000.txt");
        write_utf16(
            &path,
            "------------------------------------------------------------\n\
             Chatlog\n\
             Listener: Alpha\n\
             Session started: 2024.01.15 12:00:00\n\
             ------------------------------------------------------------\n",
        );
        assert_eq!(extract_listener(&path, true).as_deref(), Some("Alpha"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn listener_from_gamelog() {
        let dir = temp_dir();
        let path = dir.join("20240115_120000.txt");
        write_utf8(
            &path,
            "------------------------------------------------------------\n\
             Gamelog\n\
             Listener: Beta\n\
             Session started: 2024.01.15 12:00:00\n\
             ------------------------------------------------------------\n",
        );
        assert_eq!(extract_listener(&path, false).as_deref(), Some("Beta"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newest_wins_per_character() {
        let dir = temp_dir();
        let older = dir.join("Local_20240115_110000.txt");
        let newer = dir.join("Local_20240115_120000.txt");
        write_utf16(&older, "Listener: Alpha\n");
        write_utf16(&newer, "Listener: Alpha\n");
        let map = newest_logs_by_character(&dir, true);
        assert_eq!(map.get("alpha"), Some(&newer));
        fs::remove_dir_all(&dir).ok();
    }
}
