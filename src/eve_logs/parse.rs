//! Pure line parser for EVE chatlogs (UTF-16 LE) and gamelogs (UTF-8).
//! No IO. Fast-reject before the more expensive phrase matches.

use std::fmt;

/// Combat-style alert kinds. Location changes are `Event::SystemChanged`,
/// not an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertKind {
    FleetInvite,
    FollowWarp,
    Regroup,
    Compression,
    Decloak,
    CrystalBroke,
    ConvoRequest,
}

impl AlertKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertKind::FleetInvite => "fleet_invite",
            AlertKind::FollowWarp => "follow_warp",
            AlertKind::Regroup => "regroup",
            AlertKind::Compression => "compression",
            AlertKind::Decloak => "decloak",
            AlertKind::CrystalBroke => "crystal_broke",
            AlertKind::ConvoRequest => "convo_request",
        }
    }
}

impl fmt::Display for AlertKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Character jumped or Local channel changed. `timestamp` is the
    /// zero-padded `YYYY.MM.DD HH:MM:SS` string from the line (sorts
    /// lexicographically); used to pick chat vs game when both fire.
    SystemChanged {
        system: String,
        timestamp: Option<String>,
    },
    Alert {
        kind: AlertKind,
        text: String,
    },
    /// One combat-log hit. Incoming = `AMOUNT from …`; outgoing =
    /// `AMOUNT to …`. Never an alert — DPS is a rate, not a flash.
    Damage {
        incoming: bool,
        amount: f64,
    },
}

/// Cheap filter: skip player chat / combat spam before `parse_line`.
pub fn should_parse_line(line: &str, is_chatlog: bool) -> bool {
    if is_chatlog {
        contains_ignore_ascii(line, "EVE System")
    } else {
        contains_ignore_ascii(line, "Jumping")
            || contains_ignore_ascii(line, "Undocking")
            || contains_ignore_ascii(line, "(notify)")
            || contains_ignore_ascii(line, "(question)")
            || contains_ignore_ascii(line, "(mining)")
            || contains_ignore_ascii(line, "(None)")
            || contains_ignore_ascii(line, "(combat)")
    }
}

/// Decode a newly-read chunk, combining leftover encoding bytes *and*
/// a partial line from the previous read. Chatlogs are UTF-16 LE;
/// gamelogs are UTF-8. Returns complete lines without their newline.
pub fn decode_log_chunk(is_chatlog: bool, partial: &mut Vec<u8>, new_data: &[u8]) -> Vec<String> {
    partial.extend_from_slice(new_data);
    let text = if is_chatlog {
        decode_utf16_le_partial(partial)
    } else {
        decode_utf8_partial(partial)
    };
    // `partial` now holds only incomplete encoding units (odd UTF-16
    // byte, incomplete UTF-8 sequence). A trailing partial *line* has
    // to go back in as well, in front of those leftover units.
    let (lines, rem) = take_complete_lines(&text);
    if !rem.is_empty() {
        let encoding_leftover = std::mem::take(partial);
        *partial = encode_partial(is_chatlog, &rem);
        partial.extend_from_slice(&encoding_leftover);
    }
    lines
}

fn decode_utf16_le_partial(partial: &mut Vec<u8>) -> String {
    let usable = partial.len() - (partial.len() % 2);
    if usable == 0 {
        return String::new();
    }
    let u16s: Vec<u16> = partial[..usable]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let leftover = partial[usable..].to_vec();
    partial.clear();
    partial.extend_from_slice(&leftover);
    let mut s = String::from_utf16_lossy(&u16s);
    if s.starts_with('\u{FEFF}') {
        s.remove(0);
    }
    s
}

fn decode_utf8_partial(partial: &mut Vec<u8>) -> String {
    match std::str::from_utf8(partial) {
        Ok(s) => {
            let out = s.to_string();
            partial.clear();
            out
        }
        Err(e) => {
            let valid = e.valid_up_to();
            let out = String::from_utf8_lossy(&partial[..valid]).into_owned();
            let leftover_len = partial.len() - valid;
            partial.copy_within(valid.., 0);
            partial.truncate(leftover_len);
            out
        }
    }
}

/// Split decoded text into complete lines; leftover (no trailing
/// newline) is the new partial string to re-encode.
fn take_complete_lines(text: &str) -> (Vec<String>, String) {
    if text.is_empty() {
        return (Vec::new(), String::new());
    }
    let ends_with_nl = text.ends_with('\n') || text.ends_with('\r');
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    let remainder = if ends_with_nl {
        String::new()
    } else {
        lines.pop().unwrap_or_default()
    };
    lines.retain(|l| !l.is_empty());
    (lines, remainder)
}

/// Re-encode a partial line so it can live in the byte leftover buffer.
fn encode_partial(is_chatlog: bool, remainder: &str) -> Vec<u8> {
    if remainder.is_empty() {
        return Vec::new();
    }
    if is_chatlog {
        remainder
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect()
    } else {
        remainder.as_bytes().to_vec()
    }
}

pub fn parse_line(line: &str) -> Option<Event> {
    let line = normalize_line(line);
    if line.len() < 25 || line.len() > 1000 {
        return None;
    }

    if contains_ignore_ascii(&line, "EVE System") {
        return parse_local_channel(&line);
    }

    if contains_ignore_ascii(&line, "(question)") {
        return parse_fleet_invite(&line);
    }

    if contains_ignore_ascii(&line, "(notify)") {
        if let Some(ev) = parse_follow_warp(&line) {
            return Some(ev);
        }
        if let Some(ev) = parse_regroup(&line) {
            return Some(ev);
        }
        if let Some(ev) = parse_compression(&line) {
            return Some(ev);
        }
        if let Some(ev) = parse_decloak(&line) {
            return Some(ev);
        }
        if let Some(ev) = parse_crystal_broke(&line) {
            return Some(ev);
        }
        if let Some(ev) = parse_conduit(&line) {
            return Some(ev);
        }
    }

    if contains_ignore_ascii(&line, "(None)") {
        if let Some(ev) = parse_jump(&line) {
            return Some(ev);
        }
        if let Some(ev) = parse_convo(&line) {
            return Some(ev);
        }
    }

    if contains_ignore_ascii(&line, "(combat)") {
        return parse_combat(&line);
    }

    None
}

fn normalize_line(line: &str) -> String {
    line.trim()
        .trim_start_matches('\u{FEFF}')
        .trim()
        .to_string()
}

fn parse_local_channel(line: &str) -> Option<Event> {
    let marker = "Channel changed to Local";
    let idx = find_ignore_ascii(line, marker)?;
    let rest = line[idx + marker.len()..].trim();
    let rest = rest.strip_prefix(':').unwrap_or(rest).trim();
    let system = sanitize_system(rest);
    if system.is_empty() {
        return None;
    }
    Some(Event::SystemChanged {
        system,
        timestamp: timestamp_of(line),
    })
}

fn parse_jump(line: &str) -> Option<Event> {
    let marker = "Jumping from";
    let idx = find_ignore_ascii(line, marker)?;
    let rest = line[idx + marker.len()..].trim();
    let to_idx = find_ignore_ascii(rest, " to ")?;
    let to = rest[to_idx + 4..].trim();
    let system = sanitize_system(to);
    if system.is_empty() {
        return None;
    }
    Some(Event::SystemChanged {
        system,
        timestamp: timestamp_of(line),
    })
}

fn parse_conduit(line: &str) -> Option<Event> {
    find_ignore_ascii(line, "Conduit Field")?;
    let marker = "jumps you to";
    let idx = find_ignore_ascii(line, marker)?;
    let to = line[idx + marker.len()..].trim();
    let system = sanitize_system(to);
    if system.is_empty() {
        return None;
    }
    Some(Event::SystemChanged {
        system,
        timestamp: timestamp_of(line),
    })
}

fn parse_fleet_invite(line: &str) -> Option<Event> {
    find_ignore_ascii(line, "wants you to join their fleet")?;
    let stripped = strip_html(line);
    let marker = "wants you to join their fleet";
    let idx = find_ignore_ascii(&stripped, marker)?;
    let before = stripped[..idx].trim();
    // `[ ts ] (question) Name`
    let name = last_token_after_close_paren(before).unwrap_or(before);
    if name.is_empty() {
        return None;
    }
    Some(Event::Alert {
        kind: AlertKind::FleetInvite,
        text: format!("Fleet invite from {name}"),
    })
}

fn parse_follow_warp(line: &str) -> Option<Event> {
    let marker = "Following";
    let idx = find_ignore_ascii(line, marker)?;
    let rest = line[idx + marker.len()..].trim();
    let warp = find_ignore_ascii(rest, " in warp")?;
    let leader = rest[..warp].trim();
    if leader.is_empty() {
        return None;
    }
    Some(Event::Alert {
        kind: AlertKind::FollowWarp,
        text: format!("Following {leader}"),
    })
}

fn parse_regroup(line: &str) -> Option<Event> {
    let marker = "Regrouping to";
    let idx = find_ignore_ascii(line, marker)?;
    let rest = sanitize_system(&line[idx + marker.len()..]);
    if rest.is_empty() {
        return None;
    }
    Some(Event::Alert {
        kind: AlertKind::Regroup,
        text: format!("Regrouping to {rest}"),
    })
}

fn parse_compression(line: &str) -> Option<Event> {
    let marker = "Successfully compressed";
    let idx = find_ignore_ascii(line, marker)?;
    let rest = line[idx + marker.len()..].trim();
    let into = find_ignore_ascii(rest, " into ")?;
    let after = rest[into + 6..].trim();
    // `N Compressed Item` — first token is the count.
    let mut parts = after.splitn(2, char::is_whitespace);
    let count = parts.next().unwrap_or("").trim();
    let item = sanitize_system(parts.next().unwrap_or(""));
    if count.is_empty() || item.is_empty() {
        return None;
    }
    Some(Event::Alert {
        kind: AlertKind::Compression,
        text: format!("Compressed: {count}x {item}"),
    })
}

fn parse_decloak(line: &str) -> Option<Event> {
    let marker = "Your cloak deactivates due to proximity to";
    let idx = find_ignore_ascii(line, marker)?;
    let rest = line[idx + marker.len()..].trim();
    let rest = rest
        .strip_prefix("a nearby ")
        .or_else(|| rest.strip_prefix("an "))
        .unwrap_or(rest);
    let source = sanitize_system(rest);
    if source.is_empty() {
        return None;
    }
    Some(Event::Alert {
        kind: AlertKind::Decloak,
        text: format!("Decloaked by {source}"),
    })
}

fn parse_crystal_broke(line: &str) -> Option<Event> {
    let marker = "deactivates due to the destruction of the";
    let idx = find_ignore_ascii(line, marker)?;
    let rest = line[idx + marker.len()..].trim();
    let fitted = find_ignore_ascii(rest, " it was fitted with").unwrap_or(rest.len());
    let crystal = sanitize_system(&rest[..fitted]);
    if crystal.is_empty() {
        return None;
    }
    Some(Event::Alert {
        kind: AlertKind::CrystalBroke,
        text: format!("Crystal broke: {crystal}"),
    })
}

fn parse_convo(line: &str) -> Option<Event> {
    find_ignore_ascii(line, "is inviting you to a conversation")?;
    let stripped = strip_html(line);
    let marker = "is inviting you to a conversation";
    let idx = find_ignore_ascii(&stripped, marker)?;
    let before = stripped[..idx].trim();
    let name = last_token_after_close_paren(before).unwrap_or(before);
    if name.is_empty() {
        return None;
    }
    Some(Event::Alert {
        kind: AlertKind::ConvoRequest,
        text: format!("Convo from: {name}"),
    })
}

fn parse_combat(line: &str) -> Option<Event> {
    let idx = find_ignore_ascii(line, "(combat)")?;
    let after = line[idx + "(combat)".len()..].trim();
    let stripped = strip_html(after);
    let stripped = stripped.trim();
    if stripped.is_empty() {
        return None;
    }
    // Leading amount, then the first preposition: `from` = incoming,
    // `to` = outgoing. Incoming lines also contain a later `to` (the
    // victim); only the word immediately after the number counts.
    let mut end = 0;
    let mut seen_digit = false;
    let mut seen_dot = false;
    for (i, c) in stripped.char_indices() {
        if c.is_ascii_digit() {
            seen_digit = true;
            end = i + c.len_utf8();
        } else if c == '.' && !seen_dot && seen_digit {
            seen_dot = true;
            end = i + 1;
        } else {
            break;
        }
    }
    if !seen_digit {
        return None;
    }
    let amount: f64 = stripped[..end].parse().ok()?;
    if amount <= 0.0 {
        return None;
    }
    let rest = stripped[end..].trim_start();
    let incoming = if starts_word(rest, "from") {
        true
    } else if starts_word(rest, "to") {
        false
    } else {
        return None;
    };
    Some(Event::Damage { incoming, amount })
}

fn starts_word(s: &str, word: &str) -> bool {
    s.len() >= word.len()
        && s[..word.len()].eq_ignore_ascii_case(word)
        && (s.len() == word.len() || !s.as_bytes()[word.len()].is_ascii_alphanumeric())
}

fn last_token_after_close_paren(s: &str) -> Option<&str> {
    if let Some(idx) = s.rfind(')') {
        let rest = s[idx + 1..].trim();
        if !rest.is_empty() {
            return Some(rest);
        }
    }
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn sanitize_system(raw: &str) -> String {
    let stripped = strip_html(raw);
    stripped.trim_end_matches(['.', ',']).trim().to_string()
}

fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn timestamp_of(line: &str) -> Option<String> {
    let start = line.find('[')?;
    let rest = &line[start + 1..];
    let end = rest.find(']')?;
    let ts = rest[..end].trim();
    if ts.len() < 19 {
        return None;
    }
    Some(ts.to_string())
}

fn contains_ignore_ascii(hay: &str, needle: &str) -> bool {
    find_ignore_ascii(hay, needle).is_some()
}

fn find_ignore_ascii(hay: &str, needle: &str) -> Option<usize> {
    hay.as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_line(body: &str) -> String {
        format!("[ 2024.01.15 12:34:56 ] {body}")
    }

    #[test]
    fn local_channel_change() {
        let line = ts_line("EVE System > Channel changed to Local : Jita");
        match parse_line(&line) {
            Some(Event::SystemChanged { system, timestamp }) => {
                assert_eq!(system, "Jita");
                assert_eq!(timestamp.as_deref(), Some("2024.01.15 12:34:56"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn local_channel_strips_html() {
        let line = ts_line("EVE System > Channel changed to Local : <b>Jita</b>");
        match parse_line(&line) {
            Some(Event::SystemChanged { system, .. }) => assert_eq!(system, "Jita"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn jump_line() {
        let line = ts_line("(None) Jumping from Jita to Perimeter");
        match parse_line(&line) {
            Some(Event::SystemChanged { system, .. }) => assert_eq!(system, "Perimeter"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn conduit_jump() {
        let line = ts_line("(notify) A Conduit Field activated by Alice jumps you to Jita");
        match parse_line(&line) {
            Some(Event::SystemChanged { system, .. }) => assert_eq!(system, "Jita"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fleet_invite() {
        let line = ts_line(
            r#"(question) <a href="showinfo:1376//1">Bob</a> wants you to join their fleet"#,
        );
        match parse_line(&line) {
            Some(Event::Alert { kind, text }) => {
                assert_eq!(kind, AlertKind::FleetInvite);
                assert_eq!(text, "Fleet invite from Bob");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn follow_warp() {
        let line = ts_line("(notify) Following Alice in warp");
        match parse_line(&line) {
            Some(Event::Alert { kind, text }) => {
                assert_eq!(kind, AlertKind::FollowWarp);
                assert_eq!(text, "Following Alice");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn regroup() {
        let line = ts_line("(notify) Regrouping to Alice.");
        match parse_line(&line) {
            Some(Event::Alert { kind, text }) => {
                assert_eq!(kind, AlertKind::Regroup);
                assert_eq!(text, "Regrouping to Alice");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn compression() {
        let line = ts_line("(notify) Successfully compressed Veldspar into 10 Compressed Veldspar");
        match parse_line(&line) {
            Some(Event::Alert { kind, text }) => {
                assert_eq!(kind, AlertKind::Compression);
                assert_eq!(text, "Compressed: 10x Compressed Veldspar");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn decloak() {
        let line =
            ts_line("(notify) Your cloak deactivates due to proximity to a nearby stargate.");
        match parse_line(&line) {
            Some(Event::Alert { kind, text }) => {
                assert_eq!(kind, AlertKind::Decloak);
                assert_eq!(text, "Decloaked by stargate");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn crystal_broke() {
        let line = ts_line(
            "(notify) Miner II deactivates due to the destruction of the T2 Strip Miner Crystal it was fitted with",
        );
        match parse_line(&line) {
            Some(Event::Alert { kind, text }) => {
                assert_eq!(kind, AlertKind::CrystalBroke);
                assert_eq!(text, "Crystal broke: T2 Strip Miner Crystal");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn convo_request() {
        let line =
            ts_line("(None) <a href=showinfo:1373//123>Bob</a> is inviting you to a conversation.");
        match parse_line(&line) {
            Some(Event::Alert { kind, text }) => {
                assert_eq!(kind, AlertKind::ConvoRequest);
                assert_eq!(text, "Convo from: Bob");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn combat_outgoing_plain() {
        let line = ts_line("(combat) 63 to Infester Alvi - Mjolnir Light Missile - Hits");
        match parse_line(&line) {
            Some(Event::Damage {
                incoming: false,
                amount,
            }) => assert!((amount - 63.0).abs() < f64::EPSILON),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn combat_incoming_with_later_to() {
        let line = ts_line("(combat) 450 from Rifter to Bob - 150mm Railgun I - Hits");
        match parse_line(&line) {
            Some(Event::Damage {
                incoming: true,
                amount,
            }) => assert!((amount - 450.0).abs() < f64::EPSILON),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn combat_html_tagged_outgoing() {
        let line = ts_line(
            "(combat) <color=0xff00ffff><b>63</b> <color=0x77ffffff><font size=10>to</font> \
             <b><color=0xffffffff>Infester Alvi</b><font size=10><color=0x77ffffff> - \
             Mjolnir Light Missile - Hits",
        );
        match parse_line(&line) {
            Some(Event::Damage {
                incoming: false,
                amount,
            }) => assert!((amount - 63.0).abs() < f64::EPSILON),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn combat_decimal_amount() {
        let line = ts_line("(combat) 12.7 from Rifter - Light Missile - Hits");
        match parse_line(&line) {
            Some(Event::Damage {
                incoming: true,
                amount,
            }) => assert!((amount - 12.7).abs() < 1e-6),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn combat_miss_without_amount_is_ignored() {
        let line = ts_line("(combat) Your Light Missile misses Rifter completely");
        assert!(parse_line(&line).is_none());
        assert!(should_parse_line(&line, false));
    }

    #[test]
    fn notify_is_not_damage() {
        let line = ts_line("(notify) Following Alice in warp");
        match parse_line(&line) {
            Some(Event::Alert {
                kind: AlertKind::FollowWarp,
                ..
            }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn player_chat_is_ignored() {
        let line = ts_line("Bob > hello in local");
        assert!(parse_line(&line).is_none());
        assert!(!should_parse_line(&line, true));
    }

    #[test]
    fn bom_per_line_is_stripped() {
        let line = format!(
            "\u{FEFF}{}",
            ts_line("EVE System > Channel changed to Local : Dodixie")
        );
        match parse_line(&line) {
            Some(Event::SystemChanged { system, .. }) => assert_eq!(system, "Dodixie"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn utf16_chunk_round_trip_odd_split() {
        let src = "[ 2024.01.15 12:34:56 ] EVE System > Channel changed to Local : Jita\n";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xFEFFu16.to_le_bytes());
        for u in src.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        // Split mid-code-unit so leftover handling has to stitch.
        let mid = 7;
        let mut partial = Vec::new();
        let first = decode_log_chunk(true, &mut partial, &bytes[..mid]);
        assert!(first.is_empty());
        let lines = decode_log_chunk(true, &mut partial, &bytes[mid..]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Jita"));
        assert!(partial.is_empty());
    }

    #[test]
    fn utf8_partial_line_stitches() {
        let mut partial = Vec::new();
        let a = decode_log_chunk(
            false,
            &mut partial,
            b"[ 2024.01.15 12:34:56 ] (None) Jumping from Jita",
        );
        assert!(a.is_empty());
        let b = decode_log_chunk(false, &mut partial, b" to Perimeter\n");
        assert_eq!(b.len(), 1);
        match parse_line(&b[0]) {
            Some(Event::SystemChanged { system, .. }) => assert_eq!(system, "Perimeter"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn take_complete_lines_keeps_partial() {
        let (lines, rem) = take_complete_lines("one\ntwo");
        assert_eq!(lines, vec!["one"]);
        assert_eq!(rem, "two");
        let (lines, rem) = take_complete_lines("one\ntwo\n");
        assert_eq!(lines, vec!["one", "two"]);
        assert!(rem.is_empty());
    }
}
