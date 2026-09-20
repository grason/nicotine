//! Resolve EVE Chatlogs / Gamelogs directories.

use std::path::{Path, PathBuf};

use crate::config::LogsConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogDirs {
    pub chatlogs: PathBuf,
    pub gamelogs: PathBuf,
}

/// Resolve log directories. Non-empty config overrides win. Otherwise
/// Windows uses Documents/EVE/logs; Linux walks Wine prefixes of live
/// `exefile.exe` processes, then well-known Steam compatdata paths.
pub fn resolve(config: &LogsConfig) -> LogDirs {
    let chat_override = nonempty(&config.chatlog_dir);
    let game_override = nonempty(&config.gamelog_dir);
    if let (Some(c), Some(g)) = (chat_override.clone(), game_override.clone()) {
        return LogDirs {
            chatlogs: c,
            gamelogs: g,
        };
    }
    let auto = auto_dirs();
    LogDirs {
        chatlogs: chat_override.unwrap_or(auto.chatlogs),
        gamelogs: game_override.unwrap_or(auto.gamelogs),
    }
}

fn nonempty(s: &str) -> Option<PathBuf> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(PathBuf::from(t))
    }
}

fn auto_dirs() -> LogDirs {
    #[cfg(windows)]
    {
        let docs = dirs::document_dir().unwrap_or_else(|| PathBuf::from("."));
        let logs = docs.join("EVE").join("logs");
        return LogDirs {
            chatlogs: logs.join("Chatlogs"),
            gamelogs: logs.join("Gamelogs"),
        };
    }
    #[cfg(unix)]
    {
        if let Some(d) = dirs_from_live_eve_pids() {
            return d;
        }
        if let Some(d) = dirs_from_steam_compat() {
            return d;
        }
        // Still return the most common Proton path so the Alerts tab
        // has something to show even when EVE isn't running.
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        steam_compat_logs(&home.join(".steam/steam"))
            .or_else(|| steam_compat_logs(&home.join(".local/share/Steam")))
            .unwrap_or_else(|| {
                let logs = home
                    .join(".steam/steam/steamapps/compatdata/8500/pfx/drive_c/users/steamuser/Documents/EVE/logs");
                LogDirs {
                    chatlogs: logs.join("Chatlogs"),
                    gamelogs: logs.join("Gamelogs"),
                }
            })
    }
}

#[cfg(unix)]
fn dirs_from_live_eve_pids() -> Option<LogDirs> {
    for prefix in wine_prefixes_from_eve_pids() {
        if let Some(d) = logs_under_prefix(&prefix) {
            return Some(d);
        }
    }
    None
}

/// Wine prefixes of processes whose `/proc/<pid>/comm` is `exefile.exe`.
#[cfg(unix)]
fn wine_prefixes_from_eve_pids() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(proc) = std::fs::read_dir("/proc") else {
        return out;
    };
    for ent in proc.flatten() {
        let pid: u32 = match ent.file_name().to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        if !crate::eve_match::pid_is_eve_client(pid) {
            continue;
        }
        if let Some(prefix) = wine_prefix_of(pid) {
            out.push(prefix);
        }
    }
    out
}

#[cfg(unix)]
fn wine_prefix_of(pid: u32) -> Option<PathBuf> {
    let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    for kv in env.split(|b| *b == 0) {
        let Ok(s) = std::str::from_utf8(kv) else {
            continue;
        };
        if let Some(v) = s.strip_prefix("WINEPREFIX=") {
            if !v.is_empty() {
                return Some(PathBuf::from(v));
            }
        }
    }
    None
}

/// Public so tests can feed a fake prefix without touching /proc.
pub fn logs_under_prefix(prefix: &Path) -> Option<LogDirs> {
    let users = prefix.join("drive_c").join("users");
    let steamuser = users
        .join("steamuser")
        .join("Documents")
        .join("EVE")
        .join("logs");
    if steamuser.join("Chatlogs").is_dir() {
        return Some(LogDirs {
            chatlogs: steamuser.join("Chatlogs"),
            gamelogs: steamuser.join("Gamelogs"),
        });
    }
    let Ok(rd) = std::fs::read_dir(&users) else {
        return None;
    };
    for ent in rd.flatten() {
        let logs = ent.path().join("Documents").join("EVE").join("logs");
        if logs.join("Chatlogs").is_dir() {
            return Some(LogDirs {
                chatlogs: logs.join("Chatlogs"),
                gamelogs: logs.join("Gamelogs"),
            });
        }
    }
    None
}

#[cfg(unix)]
fn dirs_from_steam_compat() -> Option<LogDirs> {
    let home = dirs::home_dir()?;
    steam_compat_logs(&home.join(".steam/steam"))
        .or_else(|| steam_compat_logs(&home.join(".local/share/Steam")))
}

#[cfg(unix)]
fn steam_compat_logs(steam_root: &Path) -> Option<LogDirs> {
    let prefix = steam_root
        .join("steamapps")
        .join("compatdata")
        .join("8500")
        .join("pfx");
    logs_under_prefix(&prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins() {
        let cfg = LogsConfig {
            chatlog_dir: "/tmp/chat".into(),
            gamelog_dir: "/tmp/game".into(),
            ..LogsConfig::default()
        };
        let d = resolve(&cfg);
        assert_eq!(d.chatlogs, PathBuf::from("/tmp/chat"));
        assert_eq!(d.gamelogs, PathBuf::from("/tmp/game"));
    }

    #[test]
    fn partial_override_keeps_auto_other() {
        let cfg = LogsConfig {
            chatlog_dir: "/tmp/chat-only".into(),
            ..LogsConfig::default()
        };
        let d = resolve(&cfg);
        assert_eq!(d.chatlogs, PathBuf::from("/tmp/chat-only"));
        assert_ne!(d.gamelogs, PathBuf::from("/tmp/chat-only"));
    }

    #[test]
    fn logs_under_fake_prefix() {
        let root = std::env::temp_dir().join(format!(
            "nicotine-pfx-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let chat = root.join("drive_c/users/steamuser/Documents/EVE/logs/Chatlogs");
        let game = root.join("drive_c/users/steamuser/Documents/EVE/logs/Gamelogs");
        std::fs::create_dir_all(&chat).unwrap();
        std::fs::create_dir_all(&game).unwrap();
        let d = logs_under_prefix(&root).expect("prefix should resolve");
        assert_eq!(d.chatlogs, chat);
        assert_eq!(d.gamelogs, game);
        std::fs::remove_dir_all(&root).ok();
    }
}
