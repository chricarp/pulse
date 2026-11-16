use chrono::{Datelike, Timelike};

use super::super::ics;
use super::*;

fn t(s: &str) -> Timestamp {
    Timestamp::parse_rfc3339(s).unwrap()
}

fn rome() -> Zone {
    Zone::Named(chrono_tz::Europe::Rome)
}

fn expand(
    ics_text: &str,
    local: Zone,
    from: &str,
    to: &str,
) -> (Vec<(String, String, String)>, Vec<String>) {
    let cal = ics::parse(ics_text);
    let x = occurrences(&cal, local, t(from), t(to));
    (
        x.occs.into_iter().map(|o| (o.start.to_string(), o.end.to_string(), o.title)).collect(),
        x.warnings,
    )
}

fn occ(start: &str, end: &str, title: &str) -> (String, String, String) {
    (start.into(), end.into(), title.into())
}

#[test]
fn google_export_recurrence_overrides_cancellations_all_day() {
    let (got, warnings) = expand(
        include_str!("../../../testdata/google.ics"),
        rome(),
        "2026-11-16T00:00:00Z",
        "2026-11-30T23:59:59Z",
    );
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(
        got,
        vec![
            occ(
                "2026-11-16T09:00:00.000Z",
                "2026-11-16T11:00:00.000Z",
                "Architettura degli elaboratori"
            ),
            occ("2026-11-17T15:00:00.000Z", "2026-11-17T15:30:00.000Z", "Call with Ada"),
            // The 18th moved to the afternoon, with its own title and place.
            occ(
                "2026-11-18T13:00:00.000Z",
                "2026-11-18T15:00:00.000Z",
                "Architettura degli elaboratori (recupero)"
            ),
            // All day in the user's zone: Rome midnight to midnight.
            occ("2026-11-19T23:00:00.000Z", "2026-11-20T23:00:00.000Z", "Scadenza progetto"),
            occ(
                "2026-11-23T09:00:00.000Z",
                "2026-11-23T11:00:00.000Z",
                "Architettura degli elaboratori"
            ),
            // The 25th is cancelled; the cancelled lunch never appears.
            occ(
                "2026-11-30T09:00:00.000Z",
                "2026-11-30T11:00:00.000Z",
                "Architettura degli elaboratori"
            ),
        ]
    );
    // EXDATE, and the series before the window: 2 November is excluded, October is CEST.
    let (got, _) = expand(
        include_str!("../../../testdata/google.ics"),
        rome(),
        "2026-10-19T00:00:00Z",
        "2026-11-04T23:59:59Z",
    );
    let starts: Vec<&str> = got.iter().map(|o| o.0.as_str()).collect();
    assert_eq!(
        starts,
        [
            "2026-10-19T08:00:00.000Z",
            "2026-10-21T08:00:00.000Z",
            "2026-10-26T09:00:00.000Z",
            "2026-10-28T09:00:00.000Z",
            "2026-11-04T09:00:00.000Z"
        ]
    );
    // UNTIL ends the series.
    let (got, _) = expand(
        include_str!("../../../testdata/google.ics"),
        rome(),
        "2026-12-20T00:00:00Z",
        "2027-01-31T00:00:00Z",
    );
    assert_eq!(
        got,
        vec![occ(
            "2026-12-21T09:00:00.000Z",
            "2026-12-21T11:00:00.000Z",
            "Architettura degli elaboratori"
        )]
    );
}

#[test]
fn identities_of_moved_and_overridden_instances() {
    let cal = ics::parse(include_str!("../../../testdata/google.ics"));
    let occs = occurrences(&cal, rome(), t("2026-11-16T00:00:00Z"), t("2026-11-19T00:00:00Z")).occs;
    let keys: Vec<&str> = occs.iter().map(|o| o.key.as_str()).collect();
    let orig_18 = t("2026-11-18T09:00:00Z").as_millis();
    assert_eq!(
        keys,
        [
            format!(
                "4f1c2e8a9b3d7f0e1a2b3c4d5e@google.com@{}",
                t("2026-11-16T09:00:00Z").as_millis()
            )
            .as_str(),
            "9f8e7d6c5b4a@google.com",
            // The override keeps the identity of the instance it replaces.
            format!("4f1c2e8a9b3d7f0e1a2b3c4d5e@google.com@{orig_18}").as_str(),
        ]
    );
    assert_eq!(occs[2].location.as_deref(), Some("Aula Magna"));
    assert_eq!(occs[0].location.as_deref(), Some("Aula B, Edificio 3"));
}

#[test]
fn apple_export_weekly_monthly_all_day_and_floating() {
    let (got, warnings) = expand(
        include_str!("../../../testdata/apple.ics"),
        rome(),
        "2026-11-16T00:00:00Z",
        "2026-11-22T23:59:59Z",
    );
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(
        got,
        vec![
            occ("2026-11-16T18:00:00.000Z", "2026-11-16T19:00:00.000Z", "Gym"),
            // Floating, read in the calendar's X-WR-TIMEZONE.
            occ("2026-11-18T08:00:00.000Z", "2026-11-18T08:30:00.000Z", "Floating standup"),
            occ("2026-11-19T18:00:00.000Z", "2026-11-19T19:00:00.000Z", "Gym"),
        ]
    );
    // Monthly all-day with a date UNTIL (Apple writes it that way): through December only.
    let (got, _) = expand(
        include_str!("../../../testdata/apple.ics"),
        rome(),
        "2026-12-01T00:00:00Z",
        "2027-03-01T00:00:00Z",
    );
    let rent: Vec<&str> = got.iter().filter(|o| o.2 == "Pay rent").map(|o| o.0.as_str()).collect();
    assert_eq!(rent, ["2026-12-14T23:00:00.000Z"]);
}

#[test]
fn outlook_windows_zones_custom_zones_and_dst() {
    let (got, warnings) = expand(
        include_str!("../../../testdata/outlook.ics"),
        Zone::Named(chrono_tz::America::New_York),
        "2026-10-01T00:00:00Z",
        "2026-12-01T00:00:00Z",
    );
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(
        got,
        vec![
            // 09:00 in Berlin stays 09:00 local across the October change: 07:00Z, then 08:00Z.
            occ("2026-10-20T07:00:00.000Z", "2026-10-20T07:30:00.000Z", "Team sync"),
            occ("2026-10-27T08:00:00.000Z", "2026-10-27T08:30:00.000Z", "Team sync"),
            occ("2026-11-03T08:00:00.000Z", "2026-11-03T08:30:00.000Z", "Team sync"),
            occ("2026-11-10T08:00:00.000Z", "2026-11-10T08:30:00.000Z", "Team sync"),
            // An unnamed zone falls back to its VTIMEZONE's standard offset (+03:00).
            occ(
                "2026-11-17T09:00:00.000Z",
                "2026-11-17T10:00:00.000Z",
                "Call with the Istanbul office"
            ),
        ]
    );
}

#[test]
fn floating_without_a_calendar_zone_uses_the_users_zone_and_unknown_zones_warn() {
    let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:f\nSUMMARY:Floating\nDTSTART:20261118T090000\nDURATION:PT45M\nEND:VEVENT\nBEGIN:VEVENT\nUID:u\nSUMMARY:Unknown\nDTSTART;TZID=Nowhere/Special:20261118T090000\nEND:VEVENT\nEND:VCALENDAR\n";
    let (got, warnings) = expand(
        ics,
        Zone::Named(chrono_tz::Asia::Tokyo),
        "2026-11-17T00:00:00Z",
        "2026-11-19T00:00:00Z",
    );
    assert_eq!(
        got,
        vec![
            occ("2026-11-18T00:00:00.000Z", "2026-11-18T00:45:00.000Z", "Floating"),
            occ("2026-11-18T09:00:00.000Z", "2026-11-18T09:00:00.000Z", "Unknown"),
        ]
    );
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("Nowhere/Special"));
}

#[test]
fn durations_and_zone_names() {
    assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
    assert_eq!(parse_duration("P1W"), Some(Duration::weeks(1)));
    assert_eq!(parse_duration("P1DT12H"), Some(Duration::hours(36)));
    assert_eq!(parse_duration("-PT15M"), Some(Duration::minutes(-15)));
    assert_eq!(parse_duration("P0DT0H10M0S"), Some(Duration::minutes(10)));
    for bad in ["", "1H", "P1H", "PT1D", "P1X", "PT5"] {
        assert_eq!(parse_duration(bad), None, "{bad}");
    }
    assert_eq!(named_zone("/mozilla.org/20050126_1/Europe/Rome"), Some(chrono_tz::Europe::Rome));
    assert_eq!(named_zone("Pacific Standard Time"), Some(chrono_tz::America::Los_Angeles));
    assert_eq!(offset_zone("+0300"), Some(chrono_tz::Etc::GMTMinus3));
    assert_eq!(offset_zone("-0500"), Some(chrono_tz::Etc::GMTPlus5));
    assert_eq!(offset_zone("+0530"), None);
}

#[test]
fn a_huge_recurrence_is_capped() {
    let ics = "BEGIN:VEVENT\nUID:m\nSUMMARY:Every minute\nDTSTART:20260101T000000Z\nRRULE:FREQ=MINUTELY\nEND:VEVENT\n";
    let (got, _) = expand(ics, rome(), "2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z");
    assert_eq!(got.len(), MAX_INSTANCES_PER_EVENT);
}

#[test]
fn absurd_spans_are_clamped_instead_of_panicking() {
    // Saturates (and is clamped where it is used) rather than overflowing.
    assert_eq!(parse_duration("P99999999999999W"), Some(Duration::MAX));
    assert_eq!(parse_duration("PT9223372036854775807S"), Some(Duration::MAX));
    assert_eq!(parse_duration("P99999999999999999999999D"), None, "not a number");
    for (what, props) in [
        ("weeks", "DTSTART:20261116T090000Z\nDURATION:P99999999999999W\n"),
        ("days, recurring", "DTSTART:20261116T090000Z\nDURATION:P100000000D\nRRULE:FREQ=DAILY\n"),
        ("dtend", "DTSTART:00010101T000000Z\nDTEND:99991231T000000Z\nRRULE:FREQ=YEARLY\n"),
    ] {
        let ics = format!("BEGIN:VEVENT\nUID:x\nSUMMARY:Long\n{props}END:VEVENT\n");
        let cal = ics::parse(&ics);
        let x = occurrences(&cal, rome(), t("2026-11-16T00:00:00Z"), t("2026-11-17T00:00:00Z"));
        let (occs, warnings) = (x.occs, x.warnings);
        assert!(!occs.is_empty(), "{what}");
        for o in &occs {
            assert!(
                o.end.as_millis() - o.start.as_millis() <= MAX_SPAN.num_milliseconds(),
                "{what}"
            );
        }
        assert!(warnings.iter().any(|w| w.contains("longer than")), "{what}: {warnings:?}");
    }
}

/// The `rrule` crate's occurrences of one rule in `[from, to]`, as an oracle.
fn crate_starts(zone: chrono_tz::Tz, dtstart: &str, rule: &str, from: &str, to: &str) -> Vec<i64> {
    use rrule::{RRule, RRuleSet, Tz as RTz, Unvalidated};
    let naive = NaiveDateTime::parse_from_str(dtstart, "%Y%m%dT%H%M%S").unwrap();
    let start = zone.from_local_datetime(&naive).earliest().unwrap().with_timezone(&RTz::Tz(zone));
    let r: RRule<Unvalidated> = rule.parse().unwrap();
    let (from, to) = (t(from).as_millis(), t(to).as_millis());
    let after = DateTime::from_timestamp_millis(from - 1000).unwrap().with_timezone(&RTz::UTC);
    let before = DateTime::from_timestamp_millis(to).unwrap().with_timezone(&RTz::UTC);
    let mut starts: Vec<i64> = RRuleSet::new(start)
        .rrule(r.validate(start).unwrap())
        .after(after)
        .before(before)
        .all(u16::MAX)
        .dates
        .iter()
        .map(|d| d.timestamp_millis())
        .filter(|ms| (from..=to).contains(ms))
        .collect();
    // The crate yields an instant twice when a daylight-saving gap moves one wall time onto
    // another (02:30 and 03:30 are both 03:30); an occurrence is an instant.
    starts.sort_unstable();
    starts.dedup();
    starts
}

fn our_starts(zone: chrono_tz::Tz, dtstart: &str, rule: &str, from: &str, to: &str) -> Vec<i64> {
    let ics = format!(
        "BEGIN:VEVENT\nUID:u\nDTSTART;TZID={}:{dtstart}\nRRULE:{rule}\nEND:VEVENT\n",
        zone.name()
    );
    let x = occurrences(&ics::parse(&ics), Zone::Named(chrono_tz::UTC), t(from), t(to));
    assert_eq!((x.warnings, x.limits), (vec![], vec![]), "{rule}");
    x.occs.iter().map(|o| o.start.as_millis()).collect()
}

#[test]
fn recurrence_matches_the_rrule_crate() {
    use chrono_tz::America::New_York;
    use chrono_tz::Europe::Rome;
    let rules: &[(chrono_tz::Tz, &str, &str)] = &[
        (Rome, "20100104T090000", "FREQ=DAILY"),
        (Rome, "20100104T090000", "FREQ=DAILY;INTERVAL=3"),
        (Rome, "20260101T000000", "FREQ=DAILY;BYHOUR=1,2,3;BYMINUTE=0,30"),
        (New_York, "20260101T013000", "FREQ=DAILY"),
        (Rome, "20150603T101500", "FREQ=WEEKLY;BYDAY=MO,WE,FR;INTERVAL=2"),
        (Rome, "20200202T080000", "FREQ=WEEKLY;WKST=SU;BYDAY=SU,MO;INTERVAL=2"),
        (Rome, "20120127T180000", "FREQ=MONTHLY;BYDAY=-1FR"),
        (New_York, "20130212T120000", "FREQ=MONTHLY;BYDAY=2TU"),
        (Rome, "20110115T070000", "FREQ=MONTHLY;BYMONTHDAY=-1,15"),
        (Rome, "20110131T070000", "FREQ=MONTHLY"),
        (Rome, "20140131T170000", "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"),
        (Rome, "20120229T100000", "FREQ=YEARLY"),
        (Rome, "20100328T010000", "FREQ=YEARLY;BYMONTH=3,10;BYDAY=-1SU"),
        (Rome, "20100104T090000", "FREQ=YEARLY;BYWEEKNO=1,20,-1;BYDAY=MO,TH"),
        (New_York, "20100101T090000", "FREQ=YEARLY;BYWEEKNO=53;WKST=SU"),
        (Rome, "20100101T090000", "FREQ=YEARLY;BYYEARDAY=1,100,-1"),
        (Rome, "20100517T090000", "FREQ=YEARLY;BYDAY=20MO"),
        (Rome, "20120229T120000", "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29;BYHOUR=12"),
        (Rome, "20261101T090000", "FREQ=HOURLY;INTERVAL=5;BYHOUR=9,14,19"),
        (Rome, "20260320T003000", "FREQ=HOURLY;INTERVAL=7"),
        (Rome, "20100101T080000", "FREQ=DAILY;COUNT=5000"),
        (Rome, "20100101T080000", "FREQ=DAILY;COUNT=7000"),
        (Rome, "20140101T080000", "FREQ=WEEKLY;BYDAY=TU,TH;COUNT=1500"),
        (Rome, "20180131T080000", "FREQ=MONTHLY;BYMONTHDAY=31;COUNT=100"),
        (Rome, "20100101T080000", "FREQ=DAILY;UNTIL=20261117T000000Z"),
    ];
    let windows = [
        ("2026-11-15T00:00:00Z", "2026-11-18T00:00:00Z"),
        ("2026-03-27T00:00:00Z", "2026-03-31T00:00:00Z"),
        ("2026-10-23T00:00:00Z", "2026-11-03T00:00:00Z"),
        ("2028-02-26T00:00:00Z", "2028-03-02T00:00:00Z"),
        ("2026-12-27T00:00:00Z", "2027-01-06T00:00:00Z"),
        ("2026-05-10T00:00:00Z", "2026-05-20T00:00:00Z"),
    ];
    let mut compared = 0;
    for (zone, start, rule) in rules {
        for (from, to) in windows {
            let want = crate_starts(*zone, start, rule, from, to);
            assert_eq!(
                our_starts(*zone, start, rule, from, to),
                want,
                "{rule} from {start} in {from}..{to}"
            );
            compared += want.len();
        }
    }
    assert!(compared > 500, "the cases should exercise something: {compared}");

    // Sub-daily rules whose filters skip periods: the crate then steps by whole hours or minutes
    // and leaves the rule's grid; RFC 5545 (and python-dateutil) keep it. Checked by walking the
    // grid in UTC.
    type Keep = fn(&NaiveDateTime) -> bool;
    let grid: &[(&str, &str, i64, Keep)] = &[
        ("20261110T100000", "FREQ=MINUTELY;INTERVAL=17;BYHOUR=10", 17 * 60, |t| t.hour() == 10),
        ("20261026T000000", "FREQ=MINUTELY;INTERVAL=45;BYDAY=MO", 45 * 60, |t| {
            t.weekday() == chrono::Weekday::Mon
        }),
        ("20261101T000000", "FREQ=SECONDLY;INTERVAL=7919;BYMINUTE=5,10", 7919, |t| {
            [5, 10].contains(&t.minute())
        }),
        ("20261101T001500", "FREQ=HOURLY;INTERVAL=5;BYHOUR=9,14,19", 5 * 3600, |t| {
            [9, 14, 19].contains(&t.hour())
        }),
    ];
    for (start, rule, step, keep) in grid {
        for (from, to) in windows {
            let s = NaiveDateTime::parse_from_str(start, "%Y%m%dT%H%M%S").unwrap();
            let (lo, hi) = (t(from).as_millis(), t(to).as_millis());
            let want: Vec<i64> = (0..)
                .map(|k| s + Duration::seconds(k * step))
                .map(|n| (n, n.and_utc().timestamp_millis()))
                .take_while(|(_, ms)| *ms <= hi)
                .filter(|(n, ms)| *ms >= lo && keep(n))
                .map(|(_, ms)| ms)
                .collect();
            let got = our_starts(chrono_tz::UTC, start, rule, from, to);
            assert_eq!(got, want, "{rule} in {from}..{to}");
        }
    }
}

/// Expands one recurring event (in Rome) over three days in November 2026.
fn hostile(dtstart: &str, rule: &str) -> Expanded {
    let ics = format!(
        "BEGIN:VEVENT\nUID:h\nSUMMARY:Hostile\nDTSTART:{dtstart}\nRRULE:{rule}\nEND:VEVENT\n"
    );
    occurrences(&ics::parse(&ics), rome(), t("2026-11-16T00:00:00Z"), t("2026-11-19T00:00:00Z"))
}

#[test]
fn a_recurrence_costs_steps_bounded_by_the_window_not_by_its_history() {
    // Daily since 2010: cheap, and right.
    let x = hostile("20100104T090000Z", "FREQ=DAILY");
    assert!(x.steps <= 50, "{} steps", x.steps);
    assert_eq!(x.occs.len(), 3);
    assert_eq!(x.occs[0].start, t("2026-11-16T09:00:00Z"));
    // Never matches: costs the days of the window, not a search into the future.
    let x = hostile("20100104T090000Z", "FREQ=DAILY;BYMONTH=2;BYMONTHDAY=30");
    assert!(x.steps <= 50 && x.occs.is_empty() && x.limits.is_empty(), "{x:?}");
    let x = hostile("19000101T000000Z", "FREQ=SECONDLY;BYMONTH=2;BYMONTHDAY=30");
    assert!(x.steps <= 50 && x.occs.is_empty() && x.limits.is_empty(), "{x:?}");
    // A filter that skips most periods jumps to the next value it allows.
    let x = hostile("20261101T000000Z", "FREQ=SECONDLY;BYSECOND=0;BYMINUTE=0,30");
    assert!(x.steps <= 2_000, "{} steps", x.steps);
    // Both ends of the window count: 3 days of two an hour, plus midnight at the end.
    assert_eq!(x.occs.len(), 3 * 48 + 1);
    // Every second since 1900: the window's first occurrences, and health says what was cut.
    let x = hostile("19000101T000000Z", "FREQ=SECONDLY");
    assert!(x.steps <= MAX_EVENT_STEPS, "{} steps", x.steps);
    assert_eq!(x.occs.len(), MAX_INSTANCES_PER_EVENT);
    assert!(x.limits[0].contains("\"Hostile\" has more than 2000 occurrences"), "{:?}", x.limits);
    // A COUNT must be counted from the start, so it runs out of steps and says so.
    for rule in ["FREQ=SECONDLY;COUNT=4000000000", "FREQ=YEARLY;BYYEARDAY=366;COUNT=1000"] {
        let x = hostile("00010101T000000Z", rule);
        assert!(x.steps <= MAX_EVENT_STEPS + 10, "{rule}: {} steps", x.steps);
        assert!(x.limits[0].contains("needs more than 100000 steps"), "{rule}: {:?}", x.limits);
    }
}

#[test]
fn a_hostile_calendar_is_bounded_as_a_whole_and_the_rest_still_fires() {
    let mut ics = String::from("BEGIN:VCALENDAR\n");
    // Each fills its own occurrence budget; together they would be 3,000,000 occurrences.
    for i in 0..1_500 {
        ics.push_str(&format!(
            "BEGIN:VEVENT\nUID:m{i}\nSUMMARY:Minutely\nDTSTART:20100101T000000Z\nRRULE:FREQ=MINUTELY\nEND:VEVENT\n"
        ));
    }
    // Each spends its whole step budget; together, 200 times the calendar's.
    for i in 0..4_000 {
        ics.push_str(&format!(
            "BEGIN:VEVENT\nUID:c{i}\nSUMMARY:Counting\nDTSTART:00010101T000000Z\nRRULE:FREQ=SECONDLY;COUNT=4000000000\nEND:VEVENT\n"
        ));
    }
    // A real event, last in the file.
    ics.push_str("BEGIN:VEVENT\nUID:real\nSUMMARY:Real\nDTSTART:20261117T100000Z\nEND:VEVENT\n");
    ics.push_str("END:VCALENDAR\n");
    let x = occurrences(
        &ics::parse(&ics),
        rome(),
        t("2026-11-16T00:00:00Z"),
        t("2026-11-19T00:00:00Z"),
    );
    assert!(x.steps <= MAX_STEPS + 10, "{} steps", x.steps);
    assert!(x.occs.len() <= MAX_INSTANCES);
    assert!(x.occs.iter().any(|o| o.title == "Real"));
    let limits = x.limits.join("\n");
    assert!(limits.contains("more than 20000 occurrences"), "{limits}");
}
