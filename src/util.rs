/// 3:07, or 1:02:03 for long things.
pub fn fmt_ms(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "2 hr 5 min", "43 min", "40 sec".
pub fn fmt_duration_words(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m) = (total / 3600, (total / 60) % 60);
    match (h, m) {
        (0, 0) => format!("{total} sec"),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    }
}

pub enum SeekTarget {
    Absolute(u32),
    Forward(u32),
    Back(u32),
}

/// Accepts "83", "1:23", "1:02:03", "+15", "-10".
pub fn parse_seek(input: &str) -> Option<SeekTarget> {
    let input = input.trim();
    let (sign, rest) = match input.chars().next()? {
        '+' => (1, &input[1..]),
        '-' => (-1, &input[1..]),
        _ => (0, input),
    };
    let mut secs: u64 = 0;
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for part in parts {
        let value: u64 = part.trim().parse().ok()?;
        secs = secs * 60 + value;
    }
    let ms = u32::try_from(secs * 1000).ok()?;
    Some(match sign {
        1 => SeekTarget::Forward(ms),
        -1 => SeekTarget::Back(ms),
        _ => SeekTarget::Absolute(ms),
    })
}

/// Shortens to at most `max` characters, adding an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Escapes Discord markdown in names like "*NSYNC" or "Tyler, The Creator_".
pub fn escape_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '_' | '~' | '`' | '|' | '>' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time() {
        assert_eq!(fmt_ms(187_000), "3:07");
        assert_eq!(fmt_ms(3_723_000), "1:02:03");
        assert_eq!(fmt_duration_words(2_580_000), "43 min");
        assert_eq!(fmt_duration_words(7_500_000), "2 hr 5 min");
    }

    #[test]
    fn parses_seek() {
        assert!(matches!(
            parse_seek("83"),
            Some(SeekTarget::Absolute(83_000))
        ));
        assert!(matches!(
            parse_seek("1:23"),
            Some(SeekTarget::Absolute(83_000))
        ));
        assert!(matches!(
            parse_seek("+15"),
            Some(SeekTarget::Forward(15_000))
        ));
        assert!(matches!(
            parse_seek("-0:10"),
            Some(SeekTarget::Back(10_000))
        ));
        assert!(parse_seek("abc").is_none());
        assert!(parse_seek("").is_none());
    }

    #[test]
    fn truncates_and_escapes() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(escape_md("*NSYNC"), "\\*NSYNC");
    }
}
