//! iCalendar (RFC 5545) reading: content lines, components, and the `VEVENT` properties we use.
//!
//! Lenient where real exports are sloppy (LF instead of CRLF, unknown properties, missing
//! `VCALENDAR`), strict about bounds: the input, the number of events, the nesting depth, the
//! number of time zones, and every string kept from it are capped, because calendar content is
//! untrusted. Every content line costs a bounded number of steps (`Calendar::steps` counts
//! them), so parsing is linear in the input.

use std::collections::BTreeMap;

pub(crate) const MAX_EVENTS: usize = 20_000;
pub(crate) const MAX_TEXT: usize = 1024;
const MAX_LINE: usize = 64 * 1024;
const MAX_LIST: usize = 1_000;
/// Components nest at most this deep (real calendars use 3: `VCALENDAR`, `VEVENT`, `VALARM`).
/// Deeper ones are skipped whole.
pub(crate) const MAX_DEPTH: usize = 8;
const MAX_ZONES: usize = 1_000;

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
    /// Parser steps: one per content line plus the open components an `END` was compared
    /// with (at most [`MAX_DEPTH`]).
    pub steps: usize,
}

/// Calls `f` with each unfolded content line (a line starting with a space or tab continues the
/// previous one), at most [`MAX_LINE`] bytes; what is past that is dropped. One line is held at
/// a time.
fn unfold(text: &str, mut f: impl FnMut(&str)) {
    let mut current = String::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if !current.is_empty() {
                push_bounded(&mut current, rest);
                continue;
            }
        }
        if !current.is_empty() {
            f(&current);
            current.clear();
        }
        push_bounded(&mut current, line);
    }
    if !current.is_empty() {
        f(&current);
    }
}

fn push_bounded(out: &mut String, s: &str) {
    let room = MAX_LINE.saturating_sub(out.len());
    if s.len() <= room {
        out.push_str(s);
    } else {
        let mut end = room;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str(&s[..end]);
    }
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
    let mut p = Parser::default();
    unfold(input, |line| p.line(line));
    let mut cal = p.cal;
    if p.skipped_events > 0 {
        cal.warnings.push(format!(
            "the calendar has more than {MAX_EVENTS} events; {} were ignored",
            p.skipped_events
        ));
    }
    if p.too_deep > 0 {
        cal.warnings.push(format!(
            "{} components are nested deeper than {MAX_DEPTH} levels and were ignored",
            p.too_deep
        ));
    }
    if p.skipped_zones > 0 {
        cal.warnings.push(format!(
            "the calendar defines more than {MAX_ZONES} time zones; {} were ignored",
            p.skipped_zones
        ));
    }
    cal
}

#[derive(Default)]
struct Parser {
    cal: Calendar,
    /// The open components, innermost last; at most [`MAX_DEPTH`].
    stack: Vec<String>,
    /// Open components past [`MAX_DEPTH`]: only counted, and everything in them is ignored.
    overflow: usize,
    event: Option<VEvent>,
    tz: Option<String>,
    skipped_events: usize,
    skipped_zones: usize,
    too_deep: usize,
}

impl Parser {
    fn line(&mut self, line: &str) {
        self.cal.steps += 1;
        let Some(prop) = parse_line(line) else { return };
        let (stack, cal) = (&mut self.stack, &mut self.cal);
        match prop.name.as_str() {
            "BEGIN" => {
                if self.overflow > 0 || stack.len() == MAX_DEPTH {
                    self.overflow += 1;
                    self.too_deep += 1;
                    return;
                }
                let comp = prop.value.trim().to_ascii_uppercase();
                cal.steps += stack.len();
                if comp == "VEVENT" && !stack.iter().any(|c| c == "VEVENT") {
                    if cal.events.len() < MAX_EVENTS {
                        self.event = Some(VEvent::default());
                    } else {
                        self.skipped_events += 1;
                    }
                }
                stack.push(comp);
                return;
            }
            "END" => {
                // Closes the innermost skipped component, whatever its name: they are not
                // tracked.
                if self.overflow > 0 {
                    self.overflow -= 1;
                    return;
                }
                let comp = prop.value.trim().to_ascii_uppercase();
                cal.steps += stack.len();
                if let Some(i) = stack.iter().rposition(|c| *c == comp) {
                    stack.truncate(i);
                }
                if comp == "VEVENT" && !stack.iter().any(|c| c == "VEVENT") {
                    cal.events.extend(self.event.take());
                }
                if comp == "VTIMEZONE" {
                    self.tz = None;
                }
                return;
            }
            _ => {}
        }
        if self.overflow > 0 {
            return;
        }
        match stack.last().map(String::as_str) {
            Some("VEVENT") => {
                if let Some(ev) = self.event.as_mut() {
                    event_prop(ev, prop);
                }
            }
            Some("VTIMEZONE") if prop.name == "TZID" => {
                self.tz = Some(prop.value.trim().to_owned());
            }
            Some("STANDARD") if prop.name == "TZOFFSETTO" => {
                if let Some(id) = &self.tz {
                    if cal.tz_offsets.len() < MAX_ZONES || cal.tz_offsets.contains_key(id) {
                        cal.tz_offsets
                            .entry(id.clone())
                            .or_insert_with(|| prop.value.trim().to_owned());
                    } else {
                        self.skipped_zones += 1;
                        // Once per zone.
                        self.tz = None;
                    }
                }
            }
            Some("VCALENDAR") | None if prop.name == "X-WR-TIMEZONE" => {
                cal.default_tz = Some(prop.value.trim().chars().take(128).collect());
            }
            _ => {}
        }
    }
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
    fn nesting_is_capped_and_every_line_costs_a_bounded_number_of_steps() {
        // Unmatched BEGIN/END pairs: each END used to scan the whole stack.
        let n = 5_000;
        let mut junk =
            "BEGIN:VEVENT\nUID:before\nDTSTART:20261116T100000Z\nEND:VEVENT\n".to_owned();
        junk.push_str(&"BEGIN:A\n".repeat(n));
        junk.push_str("SUMMARY:inside the junk\n");
        junk.push_str(&"END:B\n".repeat(n));
        let cal = parse(&junk);
        let lines = junk.lines().count();
        assert!(cal.steps <= lines * (MAX_DEPTH + 2), "{} steps for {lines} lines", cal.steps);
        assert_eq!(cal.events.len(), 1);
        assert_eq!(cal.events[0].uid.as_deref(), Some("before"));
        assert!(cal.warnings.iter().any(|w| w.contains("nested")), "{:?}", cal.warnings);
        // Properties of components past the cap are not taken for an outer component's.
        let deep = format!(
            "BEGIN:VEVENT\nUID:e\n{}SUMMARY:from deep inside\n{}SUMMARY:the event\nEND:VEVENT\n",
            "BEGIN:X\n".repeat(MAX_DEPTH + 3),
            "END:X\n".repeat(MAX_DEPTH + 3)
        );
        assert_eq!(parse(&deep).events[0].summary.as_deref(), Some("the event"));
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
