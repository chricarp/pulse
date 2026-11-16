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
    let (occs, warnings) = occurrences(&cal, local, t(from), t(to));
    (
        occs.into_iter().map(|o| (o.start.to_string(), o.end.to_string(), o.title)).collect(),
        warnings,
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
    let (occs, _) = occurrences(&cal, rome(), t("2026-11-16T00:00:00Z"), t("2026-11-19T00:00:00Z"));
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
    assert_eq!(got.len(), usize::from(MAX_INSTANCES_PER_EVENT));
}
