//! Poll-based tailer. EVE keeps log files open and appends; file-change
//! notifications miss that on Windows, so we stat + read like EVE-APM.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::{AlertToggles, LogsConfig};
use crate::cycle_state::CycleState;

use super::identify::newest_logs_by_character;
use super::parse::{decode_log_chunk, parse_line, should_parse_line, AlertKind, Event};
use super::paths::resolve;
use super::state::{ActiveAlert, DpsTracker, LogLiveState};

const FAST_POLL: Duration = Duration::from_millis(500);
const SLOW_POLL: Duration = Duration::from_millis(1000);
const FAST_MOMENTUM_TICKS: u32 = 10;
const DIR_RESCAN: Duration = Duration::from_secs(30);
const INITIAL_TAIL: u64 = 64 * 1024;

struct FileState {
    path: PathBuf,
    character: String,
    is_chatlog: bool,
    offset: u64,
    last_size: u64,
    partial: Vec<u8>,
}

/// Per-character location clock so a later gamelog jump overrides a
/// stale chatlog Local-channel line (and vice versa). Timestamps are
/// zero-padded `YYYY.MM.DD HH:MM:SS` and sort lexicographically.
struct LocationClock {
    timestamp: String,
    system: String,
}

pub fn spawn(
    logs: Arc<Mutex<LogsConfig>>,
    cycle: Arc<Mutex<CycleState>>,
    live: Arc<Mutex<LogLiveState>>,
) -> JoinHandle<()> {
    thread::spawn(move || run(logs, cycle, live))
}

fn run(
    logs: Arc<Mutex<LogsConfig>>,
    cycle: Arc<Mutex<CycleState>>,
    live: Arc<Mutex<LogLiveState>>,
) {
    let mut files: HashMap<PathBuf, FileState> = HashMap::new();
    let mut clocks: HashMap<String, LocationClock> = HashMap::new();
    let mut dps = DpsTracker::default();
    let mut last_scan = Instant::now()
        .checked_sub(DIR_RESCAN)
        .unwrap_or_else(Instant::now);
    let mut last_cfg: Option<LogsConfig> = None;
    let mut momentum: u32 = 0;
    let mut last_enabled = false;

    loop {
        let cfg = logs.lock().unwrap().clone();
        let enabled = cfg.enabled;
        if enabled != last_enabled {
            if enabled {
                println!("Log monitor started");
            } else {
                println!("Log monitor stopped");
                files.clear();
                dps = DpsTracker::default();
                live.lock().unwrap().dps.clear();
            }
            last_enabled = enabled;
        }

        if !enabled {
            thread::sleep(SLOW_POLL);
            continue;
        }

        let cfg_changed = last_cfg.as_ref() != Some(&cfg);
        let due = last_scan.elapsed() >= DIR_RESCAN;
        if cfg_changed || due || files.is_empty() {
            let names = character_filter(&cycle);
            rescan(&cfg, &names, &mut files, &mut clocks, &live);
            last_scan = Instant::now();
            last_cfg = Some(cfg.clone());
        }

        let mut had_relevant = false;
        for state in files.values_mut() {
            match read_new(state) {
                Ok(lines) => {
                    for line in lines {
                        if !should_parse_line(&line, state.is_chatlog) {
                            continue;
                        }
                        let Some(event) = parse_line(&line) else {
                            continue;
                        };
                        had_relevant = true;
                        apply_event(&state.character, event, &cfg, &mut clocks, &mut dps, &live);
                    }
                }
                Err(e) => debug_log(format_args!("log tail {}: {e}", state.path.display())),
            }
        }

        {
            let now = Instant::now();
            let mut g = live.lock().unwrap();
            g.prune_expired(now);
            dps.flush_into(&mut g, now);
        }

        if had_relevant {
            momentum = FAST_MOMENTUM_TICKS;
        } else {
            momentum = momentum.saturating_sub(1);
        }
        let sleep = if momentum > 0 { FAST_POLL } else { SLOW_POLL };
        thread::sleep(sleep);
    }
}

fn character_filter(cycle: &Arc<Mutex<CycleState>>) -> Vec<String> {
    cycle
        .lock()
        .unwrap()
        .character_order()
        .map(|s| s.to_vec())
        .unwrap_or_default()
}

fn rescan(
    cfg: &LogsConfig,
    names: &[String],
    files: &mut HashMap<PathBuf, FileState>,
    clocks: &mut HashMap<String, LocationClock>,
    live: &Arc<Mutex<LogLiveState>>,
) {
    let dirs = resolve(cfg);
    {
        let mut g = live.lock().unwrap();
        g.resolved_chatlog_dir = Some(dirs.chatlogs.clone());
        g.resolved_gamelog_dir = Some(dirs.gamelogs.clone());
    }

    let mut wanted: HashMap<PathBuf, (String, bool)> = HashMap::new();
    collect_wanted(&dirs.chatlogs, true, names, &mut wanted);
    collect_wanted(&dirs.gamelogs, false, names, &mut wanted);

    files.retain(|p, _| wanted.contains_key(p));

    for (path, (character, is_chatlog)) in wanted {
        if files.contains_key(&path) {
            continue;
        }
        match open_initial(&path, character.clone(), is_chatlog, clocks, live) {
            Ok(state) => {
                debug_log(format_args!(
                    "monitoring {} log for {character}: {}",
                    if is_chatlog { "chat" } else { "game" },
                    path.display()
                ));
                files.insert(path, state);
            }
            Err(e) => debug_log(format_args!("skip {}: {e}", path.display())),
        }
    }
}

fn collect_wanted(
    dir: &Path,
    chatlogs: bool,
    names: &[String],
    wanted: &mut HashMap<PathBuf, (String, bool)>,
) {
    let map = newest_logs_by_character(dir, chatlogs);
    if names.is_empty() {
        for (key, path) in map {
            // `key` is lowercase; recover display name from the Listener
            // header so preview matching stays case-insensitive either way.
            if let Some(listener) = super::identify::extract_listener(&path, chatlogs) {
                wanted.insert(path, (listener, chatlogs));
            } else {
                wanted.insert(path, (key, chatlogs));
            }
        }
        return;
    }
    for name in names {
        let key = name.to_lowercase();
        if let Some(path) = map.get(&key) {
            wanted.insert(path.clone(), (name.clone(), chatlogs));
        }
    }
}

fn open_initial(
    path: &Path,
    character: String,
    is_chatlog: bool,
    clocks: &mut HashMap<String, LocationClock>,
    live: &Arc<Mutex<LogLiveState>>,
) -> std::io::Result<FileState> {
    let meta = fs::metadata(path)?;
    let size = meta.len();
    let start = size.saturating_sub(INITIAL_TAIL);
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let mut partial = Vec::new();
    let mut lines = decode_log_chunk(is_chatlog, &mut partial, &buf);
    // Flush a trailing partial line so a system change at EOF still counts.
    if !partial.is_empty() {
        let nl: &[u8] = if is_chatlog { &[0x0A, 0x00] } else { b"\n" };
        lines.extend(decode_log_chunk(is_chatlog, &mut partial, nl));
    }

    // Walk newest-first for the latest location; do not fire alerts
    // from historical lines.
    for line in lines.iter().rev() {
        if !should_parse_line(line, is_chatlog) {
            continue;
        }
        if let Some(Event::SystemChanged { system, timestamp }) = parse_line(line) {
            apply_system(&character, system, timestamp, clocks, live);
            break;
        }
    }

    Ok(FileState {
        path: path.to_path_buf(),
        character,
        is_chatlog,
        offset: size,
        last_size: size,
        partial: Vec::new(),
    })
}

fn read_new(state: &mut FileState) -> std::io::Result<Vec<String>> {
    let meta = fs::metadata(&state.path)?;
    let size = meta.len();
    if size == state.last_size {
        return Ok(Vec::new());
    }
    if size < state.last_size {
        debug_log(format_args!(
            "log truncated, resetting: {}",
            state.path.display()
        ));
        state.offset = 0;
        state.partial.clear();
    }
    let mut file = File::open(&state.path)?;
    if state.offset > size {
        state.offset = 0;
        state.partial.clear();
    }
    file.seek(SeekFrom::Start(state.offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    state.offset = size;
    state.last_size = size;
    if buf.is_empty() {
        return Ok(Vec::new());
    }
    Ok(decode_log_chunk(state.is_chatlog, &mut state.partial, &buf))
}

fn apply_event(
    character: &str,
    event: Event,
    cfg: &LogsConfig,
    clocks: &mut HashMap<String, LocationClock>,
    dps: &mut DpsTracker,
    live: &Arc<Mutex<LogLiveState>>,
) {
    match event {
        Event::SystemChanged { system, timestamp } => {
            apply_system(character, system, timestamp, clocks, live);
        }
        Event::Damage { incoming, amount } => {
            let now = Instant::now();
            if incoming {
                live.lock().unwrap().stamp_incoming(character, now);
            }
            if cfg.show_dps {
                dps.add(character, incoming, amount, now);
            }
        }
        Event::Alert { kind, text } => {
            if !alert_enabled(&cfg.alerts, kind) {
                return;
            }
            let secs = cfg.alert_secs.max(1) as u64;
            let alert = ActiveAlert {
                kind,
                text: text.clone(),
                expires_at: Instant::now() + Duration::from_secs(secs),
            };
            debug_log(format_args!(
                "alert {character}: {} — {text}",
                kind.as_str()
            ));
            live.lock()
                .unwrap()
                .alerts
                .insert(character.to_string(), alert);
        }
    }
}

fn apply_system(
    character: &str,
    system: String,
    timestamp: Option<String>,
    clocks: &mut HashMap<String, LocationClock>,
    live: &Arc<Mutex<LogLiveState>>,
) {
    let ts = timestamp.unwrap_or_default();
    if let Some(prev) = clocks.get(character) {
        if prev.system == system {
            return;
        }
        if !ts.is_empty() && !prev.timestamp.is_empty() && ts < prev.timestamp {
            return;
        }
    }
    clocks.insert(
        character.to_string(),
        LocationClock {
            timestamp: ts,
            system: system.clone(),
        },
    );
    debug_log(format_args!("system {character}: {system}"));
    live.lock()
        .unwrap()
        .systems
        .insert(character.to_string(), system);
}

fn alert_enabled(toggles: &AlertToggles, kind: AlertKind) -> bool {
    match kind {
        AlertKind::FleetInvite => toggles.fleet_invite,
        AlertKind::FollowWarp => toggles.follow_warp,
        AlertKind::Regroup => toggles.regroup,
        AlertKind::Compression => toggles.compression,
        AlertKind::Decloak => toggles.decloak,
        AlertKind::CrystalBroke => toggles.crystal_broke,
        AlertKind::ConvoRequest => toggles.convo_request,
    }
}

fn debug_log(args: std::fmt::Arguments) {
    if std::env::var_os("NICOTINE_DEBUG_INPUT").is_some() {
        eprintln!("eve_logs: {args}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_log() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "nicotine-tail-{}-{}.txt",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        File::create(&p).unwrap();
        p
    }

    fn append(path: &Path, s: &str) {
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn reads_appended_lines_and_ignores_unchanged() {
        let path = temp_log();
        append(
            &path,
            "[ 2024.01.15 12:00:00 ] (None) Jumping from Jita to Perimeter\n",
        );
        let mut state = FileState {
            path: path.clone(),
            character: "Alpha".into(),
            is_chatlog: false,
            offset: 0,
            last_size: 0,
            partial: Vec::new(),
        };
        let lines = read_new(&mut state).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(parse_line(&lines[0]).is_some());
        let again = read_new(&mut state).unwrap();
        assert!(again.is_empty());
        append(
            &path,
            "[ 2024.01.15 12:01:00 ] (notify) Following Alice in warp\n",
        );
        let more = read_new(&mut state).unwrap();
        assert_eq!(more.len(), 1);
        match parse_line(&more[0]) {
            Some(Event::Alert {
                kind: AlertKind::FollowWarp,
                ..
            }) => {}
            other => panic!("{other:?}"),
        }
        fs::remove_file(&path).ok();
    }

    #[test]
    fn truncate_resets_offset() {
        let path = temp_log();
        append(
            &path,
            "[ 2024.01.15 12:00:00 ] (notify) Following Alice in warp\n",
        );
        let mut state = FileState {
            path: path.clone(),
            character: "Alpha".into(),
            is_chatlog: false,
            offset: 0,
            last_size: 0,
            partial: Vec::new(),
        };
        let _ = read_new(&mut state).unwrap();
        fs::write(
            &path,
            "[ 2024.01.15 12:02:00 ] (notify) Following Bob in warp\n",
        )
        .unwrap();
        let lines = read_new(&mut state).unwrap();
        assert_eq!(lines.len(), 1);
        match parse_line(&lines[0]) {
            Some(Event::Alert { text, .. }) => assert!(text.contains("Bob")),
            other => panic!("{other:?}"),
        }
        fs::remove_file(&path).ok();
    }

    #[test]
    fn initial_open_sets_system_without_alerts() {
        let path = temp_log();
        append(
            &path,
            "[ 2024.01.15 12:00:00 ] (notify) Following Alice in warp\n\
             [ 2024.01.15 12:01:00 ] (None) Jumping from Jita to Perimeter\n",
        );
        let live = Arc::new(Mutex::new(LogLiveState::default()));
        let mut clocks = HashMap::new();
        let state = open_initial(&path, "Alpha".into(), false, &mut clocks, &live).unwrap();
        let g = live.lock().unwrap();
        assert_eq!(
            g.systems.get("Alpha").map(String::as_str),
            Some("Perimeter")
        );
        assert!(g.alerts.is_empty(), "historical alerts must not fire");
        assert_eq!(state.offset, fs::metadata(&path).unwrap().len());
        fs::remove_file(&path).ok();
    }
}
