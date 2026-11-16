//! iCalendar (RFC 5545) reading: content lines, components, and the `VEVENT` properties we use.
//!
//! Lenient where real exports are sloppy (LF instead of CRLF, unknown properties, missing
//! `VCALENDAR`), strict about bounds: the input, the number of events, and every string kept
//! from it are capped, because calendar content is untrusted.

use std::collections::BTreeMap;

pub(crate) const MAX_EVENTS: usize = 20_000;
pub(crate) const MAX_TEXT: usize = 1024;
const MAX_LINE: usize = 64 * 1024;
const MAX_LIST: usize = 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prop {
    pub name: String,
    pub params: BTreeMap<String, String>,
    pub value: String,
}

impl Prop {
    pub(crate) fn param(&self, name: &str) -> Option<&str> {
        self.params.get(name).map(String::as_str)
    }
}

/// One `VEVENT`, with only what we need. Text values are unescaped and bounded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct VEvent {
    pub uid: Option<String>,
    pub summary: Option<String>,
    pub location: Option<String>,
    pub status: Option<String>,
    pub dtstart: Option<Prop>,
    pub dtend: Option<Prop>,
    pub duration: Option<String>,
    pub rrules: Vec<String>,
    pub rdates: Vec<Prop>,
    pub exdates: Vec<Prop>,
    pub recurrence_id: Option<Prop>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Calendar {
    pub events: Vec<VEvent>,
    /// `TZID` → the `TZOFFSETTO` of its first `STANDARD` block, for zones we cannot name.
    pub tz_offsets: BTreeMap<String, String>,
    /// `X-WR-TIMEZONE` (Google): the calendar's zone, used for floating times.
    pub default_tz: Option<String>,
    pub warnings: Vec<String>,
}

/// Unfolds content lines (a line starting with a space or tab continues the previous one).
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                if last.len() + rest.len() <= MAX_LINE {
                    last.push_str(rest);
                }
                continue;
            }
        }
        if !line.is_empty() {
            out.push(line.chars().take(MAX_LINE).collect());
        }
    }
    out
}

/// `NAME;P1=v;P2="quoted;value":value`.
pub(crate) fn parse_line(line: &str) -> Option<Prop> {
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = split_unquoted(head, ';').into_iter();
    let name = parts.next()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let mut params = BTreeMap::new();
    for p in parts {
        if let Some((k, v)) = p.split_once('=') {
            params.insert(k.trim().to_ascii_uppercase(), v.trim().trim_matches('"').to_owned());
        }
    }
    Some(Prop { name, params, value: value.to_owned() })
}

fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == sep && !in_quotes {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    out.push(&s[start..]);
    out
}

/// TEXT unescaping (`\n`, `\,`, `\;`, `\\`), with control characters other than newline
/// dropped, cut to `MAX_TEXT` bytes.
pub(crate) fn text(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        let c = if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => '\n',
                Some(other) => other,
                None => break,
            }
        } else {
            c
        };
        if c.is_control() && c != '\n' {
            continue;
        }
        if out.len() + c.len_utf8() > MAX_TEXT {
            break;
        }
        out.push(c);
    }
    out.trim().to_owned()
}

pub(crate) fn parse(input: &str) -> Calendar {
    let mut cal = Calendar::default();
    // The component stack, innermost last.
    let mut stack: Vec<String> = Vec::new();
    let mut event: Option<VEvent> = None;
    let mut tz: Option<String> = None;
    let mut skipped_events = 0usize;
    for line in unfold(input) {
        let Some(prop) = parse_line(&line) else { continue };
        match prop.name.as_str() {
            "BEGIN" => {
                let comp = prop.value.trim().to_ascii_uppercase();
                if comp == "VEVENT" && stack.last().map(String::as_str) != Some("VEVENT") {
                    if cal.events.len() < MAX_EVENTS {
                        event = Some(VEvent::default());
                    } else {
                        skipped_events += 1;
                    }
                }
                stack.push(comp);
                continue;
            }
            "END" => {
                let comp = prop.value.trim().to_ascii_uppercase();
                if let Some(i) = stack.iter().rposition(|c| *c == comp) {
                    stack.truncate(i);
                }
                if comp == "VEVENT" && !stack.iter().any(|c| c == "VEVENT") {
                    cal.events.extend(event.take());
                }
                if comp == "VTIMEZONE" {
                    tz = None;
                }
                continue;
            }
            _ => {}
        }
        match stack.last().map(String::as_str) {
            Some("VEVENT") => {
                if let Some(ev) = event.as_mut() {
                    event_prop(ev, prop);
                }
            }
            Some("VTIMEZONE") if prop.name == "TZID" => tz = Some(prop.value.trim().to_owned()),
            Some("STANDARD") if prop.name == "TZOFFSETTO" => {
                if let Some(id) = &tz {
                    cal.tz_offsets
                        .entry(id.clone())
                        .or_insert_with(|| prop.value.trim().to_owned());
                }
            }
            Some("VCALENDAR") | None if prop.name == "X-WR-TIMEZONE" => {
                cal.default_tz = Some(prop.value.trim().chars().take(128).collect());
            }
            _ => {}
        }
    }
    if skipped_events > 0 {
        cal.warnings.push(format!(
            "the calendar has more than {MAX_EVENTS} events; {skipped_events} were ignored"
        ));
    }
    cal
}

fn event_prop(ev: &mut VEvent, prop: Prop) {
    match prop.name.as_str() {
        "UID" => ev.uid = Some(text(&prop.value)),
        "SUMMARY" => ev.summary = Some(text(&prop.value)),
        "LOCATION" => ev.location = Some(text(&prop.value)),
        "STATUS" => ev.status = Some(prop.value.trim().to_ascii_uppercase()),
        "DTSTART" => ev.dtstart = Some(prop),
        "DTEND" => ev.dtend = Some(prop),
        "DURATION" => ev.duration = Some(prop.value.trim().to_owned()),
        "RRULE" if ev.rrules.len() < 8 => ev.rrules.push(prop.value.trim().to_owned()),
        "RDATE" if ev.rdates.len() < MAX_LIST => ev.rdates.push(prop),
        "EXDATE" if ev.exdates.len() < MAX_LIST => ev.exdates.push(prop),
        "RECURRENCE-ID" => ev.recurrence_id = Some(prop),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_params_and_text() {
        let p = parse_line("DTSTART;TZID=\"America/New_York\";VALUE=DATE-TIME:20261116T100000")
            .unwrap();
        assert_eq!(p.name, "DTSTART");
        assert_eq!(p.param("TZID"), Some("America/New_York"));
        assert_eq!(p.value, "20261116T100000");
        let p = parse_line("ATTENDEE;CN=\"Doe; Jane: PhD\":mailto:jane@example.com").unwrap();
        assert_eq!(p.param("CN"), Some("Doe; Jane: PhD"));
        assert_eq!(p.value, "mailto:jane@example.com");
        assert_eq!(text(r"Lunch\, then\nreview\; \\done"), "Lunch, then\nreview; \\done");
        assert_eq!(text("bell\u{7}\u{1b}[31m red"), "bell[31m red");
        assert_eq!(text(&"é".repeat(2000)).len(), MAX_TEXT);
        assert!(parse_line("no colon here").is_none());
    }

    #[test]
    fn unfolding_nesting_and_bounds() {
        let ics = "BEGIN:VCALENDAR\r\nX-WR-TIMEZONE:Europe/Rome\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:A very long\r\n  title that is folded\r\nBEGIN:VALARM\r\nSUMMARY:not the event title\r\nEND:VALARM\r\nDTSTART:20261116T100000Z\r\nEND:VEVENT\r\nBEGIN:VTIMEZONE\r\nTZID:Custom Zone\r\nBEGIN:DAYLIGHT\r\nTZOFFSETTO:+0200\r\nEND:DAYLIGHT\r\nBEGIN:STANDARD\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nEND:VCALENDAR\r\n";
        let cal = parse(ics);
        assert_eq!(cal.events.len(), 1);
        assert_eq!(cal.events[0].summary.as_deref(), Some("A very long title that is folded"));
        assert_eq!(cal.default_tz.as_deref(), Some("Europe/Rome"));
        assert_eq!(cal.tz_offsets.get("Custom Zone").map(String::as_str), Some("+0100"));
        // LF-only, no VCALENDAR, junk lines: still read.
        let cal = parse("garbage\nBEGIN:VEVENT\nUID:b\nDTSTART:20261116\nEND:VEVENT\n");
        assert_eq!(cal.events[0].uid.as_deref(), Some("b"));
        let many: String =
            (0..MAX_EVENTS + 5).map(|i| format!("BEGIN:VEVENT\nUID:{i}\nEND:VEVENT\n")).collect();
        let cal = parse(&many);
        assert_eq!(cal.events.len(), MAX_EVENTS);
        assert_eq!(cal.warnings.len(), 1);
    }
}
