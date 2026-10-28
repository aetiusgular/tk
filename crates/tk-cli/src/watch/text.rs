//! Text that reaches your terminal. Every field `tk watch` prints came from
//! somewhere else: an event's summary and detail are written by a harness, a
//! hook or a chat token, which is the agent's too (rules case 66). A summary
//! can carry escape sequences that move the cursor, rewrite the title, set
//! the clipboard (OSC 52) or draw a fake prompt, and bidirectional controls
//! that reorder what you read (Trojan Source, CVE-2021-42574). So nothing is
//! printed as it came: `clean` turns every control and format character into
//! a visible `\u{..}` escape before it reaches the terminal.

/// One line, safe to print: C0 and C1 controls, DEL, line and paragraph
/// separators, bidi embeddings, overrides and isolates, zero-width marks and
/// the BOM become `\u{XXXX}`; the result is cut at `max` characters with
/// `…`. Tabs become one space.
pub fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut shown = 0usize;
    for c in text.chars() {
        if shown >= max {
            out.push('…');
            return out;
        }
        if c == '\t' {
            out.push(' ');
            shown += 1;
        } else if hidden(c) {
            let esc = format!("\\u{{{:X}}}", c as u32);
            shown += esc.chars().count();
            out.push_str(&esc);
        } else {
            out.push(c);
            shown += 1;
        }
    }
    out
}

/// Characters a terminal acts on or a reader cannot see.
pub fn hidden(c: char) -> bool {
    let u = c as u32;
    u < 0x20
        || (0x7F..=0x9F).contains(&u)
        || matches!(
            u,
            0x061C // Arabic letter mark
                | 0x180E // Mongolian vowel separator
                | 0x200B..=0x200F // zero-width space, joiners, LRM, RLM
                | 0x2028..=0x202E // line and paragraph separators, embeddings, overrides
                | 0x2060..=0x2064 // word joiner, invisible operators
                | 0x2066..=0x2069 // isolates
                | 0xFEFF // BOM, zero-width no-break space
                | 0xFFF9..=0xFFFB // interlinear annotation
        )
}

/// `HH:MM:SS` in local time for an epoch-milliseconds stamp.
pub fn clock(ms: i64) -> String {
    match local(ms) {
        Some(t) => format!("{:02}:{:02}:{:02}", t.tm_hour, t.tm_min, t.tm_sec),
        None => "--:--:--".into(),
    }
}

/// `28 Oct 10:42` in local time.
pub fn date_time(ms: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    match local(ms) {
        Some(t) => format!(
            "{} {} {:02}:{:02}",
            t.tm_mday,
            MONTHS[t.tm_mon.clamp(0, 11) as usize],
            t.tm_hour,
            t.tm_min
        ),
        None => format!("{ms} ms"),
    }
}

fn local(ms: i64) -> Option<libc::tm> {
    let secs = ms.div_euclid(1000) as libc::time_t;
    // SAFETY: an all-zero `tm` is a valid value for localtime_r to fill;
    // both pointers are to live locals.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::localtime_r(&secs, &mut tm) };
    (!r.is_null()).then_some(tm)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `--since` and `--until`: `30s`, `10m`, `2h`, `7d` before `now`, `now`,
/// or a plain number of milliseconds since the epoch.
pub fn parse_when(text: &str, now: i64) -> Option<i64> {
    let t = text.trim();
    if t == "now" {
        return Some(now);
    }
    if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
        return t.parse::<i64>().ok();
    }
    let (num, unit) = t.split_at(t.len().checked_sub(1)?);
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = num.parse().ok()?;
    let per = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    now.checked_sub(n.checked_mul(per)?)
}

/// `12,345`.
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The first `n` characters of an id, for a column.
pub fn short(id: &str, n: usize) -> String {
    clean(&id.chars().take(n).collect::<String>(), n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_never_reach_the_terminal() {
        // CSI clear screen, OSC 52 clipboard write, OSC 0 title, BEL.
        let evil = "ok\x1b[2J\x1b]52;c;ZWNobyBoaQ==\x07\x1b]0;tk: approved\x07";
        let c = clean(evil, 500);
        assert!(!c.chars().any(|ch| ch == '\x1b' || ch == '\x07'), "{c}");
        assert!(c.starts_with("ok\\u{1B}[2J"), "{c}");
    }

    #[test]
    fn c1_controls_and_del_are_escaped() {
        // 0x9B is a one-byte CSI in some terminals.
        let c = clean("a\u{9b}31mb\u{7f}", 100);
        assert_eq!(c, "a\\u{9B}31mb\\u{7F}");
    }

    #[test]
    fn bidi_controls_are_visible() {
        // Trojan Source: an override that makes "deny" read as "allow".
        let c = clean("tool \u{202E}wolla\u{202C} x\u{2066}y\u{2069}", 100);
        assert!(c.contains("\\u{202E}") && c.contains("\\u{202C}"));
        assert!(c.contains("\\u{2066}") && c.contains("\\u{2069}"));
        assert!(!c.chars().any(hidden));
    }

    #[test]
    fn newlines_cannot_forge_a_second_line() {
        let c = clean("one\nAllow? [y/N] y\r\u{2028}", 100);
        assert!(!c.contains('\n') && !c.contains('\r') && !c.contains('\u{2028}'));
    }

    #[test]
    fn zero_width_and_bom_are_shown() {
        assert_eq!(
            clean("a\u{200b}b\u{feff}", 100),
            "a\\u{200B}b\\u{FEFF}"
        );
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis() {
        let c = clean(&"x".repeat(50), 10);
        assert_eq!(c, format!("{}…", "x".repeat(10)));
        // An escape counts towards the limit, so a flood of controls is cut.
        let c = clean(&"\x1b".repeat(1000), 20);
        assert!(c.chars().count() <= 21 + 7, "{}", c.len());
    }

    #[test]
    fn plain_text_and_other_scripts_pass() {
        assert_eq!(clean("exa/search ok, 120 ms", 100), "exa/search ok, 120 ms");
        assert_eq!(clean("ファイル 書き込み", 100), "ファイル 書き込み");
        assert_eq!(clean("a\tb", 100), "a b");
    }

    #[test]
    fn when_parses_durations_numbers_and_now() {
        let now = 10_000_000;
        assert_eq!(parse_when("30s", now), Some(now - 30_000));
        assert_eq!(parse_when("10m", now), Some(now - 600_000));
        assert_eq!(parse_when("2h", now), Some(now - 7_200_000));
        assert_eq!(parse_when("1d", 100_000_000), Some(100_000_000 - 86_400_000));
        assert_eq!(parse_when("now", now), Some(now));
        assert_eq!(parse_when("1792204985350", now), Some(1_792_204_985_350));
        for bad in ["", "m", "-5m", "5w", "5 m", "1e3", "99999999999999999999d"] {
            assert_eq!(parse_when(bad, now), None, "{bad}");
        }
    }

    #[test]
    fn counts_have_thousands_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn clock_is_two_digit_fields() {
        let c = clock(1_792_204_985_350);
        assert_eq!(c.len(), 8);
        assert_eq!(&c[2..3], ":");
    }
}
