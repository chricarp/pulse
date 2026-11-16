use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use pulse_contracts::NewEvent;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::clock::TokioClock;

fn t(s: &str) -> Timestamp {
    Timestamp::parse_rfc3339(s).unwrap()
}

fn occurrence(key: &str, start: &str, end: &str) -> Occurrence {
    Occurrence {
        key: key.into(),
        uid: key.into(),
        title: key.into(),
        location: None,
        start: t(start),
        end: t(end),
    }
}

#[test]
fn planner_fires_once_per_occurrence_and_kind() {
    let mut p = Planner { lead: Duration::from_mins(10), fired: BTreeMap::new(), steps: 0 };
    let a = occurrence("a", "2026-11-16T10:00:00Z", "2026-11-16T11:00:00Z");
    let show = |fires: Vec<(Kind, Occurrence)>| {
        fires.into_iter().map(|(k, o)| (k, o.key)).collect::<Vec<_>>()
    };

    let (fires, next) = p.due(std::slice::from_ref(&a), t("2026-11-16T09:00:00Z"));
    assert_eq!((show(fires), next), (vec![], Some(t("2026-11-16T09:50:00Z"))));
    let (fires, next) = p.due(std::slice::from_ref(&a), t("2026-11-16T09:50:00Z"));
    assert_eq!(
        (show(fires), next),
        (vec![(Kind::Starting, "a".into())], Some(t("2026-11-16T11:00:00Z")))
    );
    // Looked at again (a poll, a timer): nothing new.
    assert_eq!(show(p.due(std::slice::from_ref(&a), t("2026-11-16T09:51:00Z")).0), vec![]);
    // Moved later after it fired: `starting` does not fire again; `ended` follows the move.
    let moved = occurrence("a", "2026-11-16T15:00:00Z", "2026-11-16T16:00:00Z");
    let (fires, next) = p.due(std::slice::from_ref(&moved), t("2026-11-16T11:00:00Z"));
    assert_eq!((show(fires), next), (vec![], Some(t("2026-11-16T16:00:00Z"))));
    assert_eq!(show(p.due(&[moved], t("2026-11-16T16:00:00Z")).0), vec![(Kind::Ended, "a".into())]);

    // Removed before its time: never fires.
    let mut p = Planner { lead: Duration::from_mins(10), fired: BTreeMap::new(), steps: 0 };
    let b = occurrence("b", "2026-11-16T10:00:00Z", "2026-11-16T11:00:00Z");
    let _ = p.due(std::slice::from_ref(&b), t("2026-11-16T09:00:00Z"));
    assert_eq!(show(p.due(&[], t("2026-11-16T12:00:00Z")).0), vec![]);

    // Slept through the start: late within the grace period, dropped after it.
    let mut p = Planner { lead: Duration::from_mins(10), fired: BTreeMap::new(), steps: 0 };
    assert_eq!(
        show(p.due(std::slice::from_ref(&b), t("2026-11-16T10:10:00Z")).0),
        vec![(Kind::Starting, "b".into())]
    );
    let mut p = Planner { lead: Duration::from_mins(10), fired: BTreeMap::new(), steps: 0 };
    let (fires, next) = p.due(std::slice::from_ref(&b), t("2026-11-16T10:20:00Z"));
    assert_eq!((show(fires), next), (vec![], Some(t("2026-11-16T11:00:00Z"))));
    // Ended long ago (a first start in the afternoon): nothing.
    let mut p = Planner { lead: Duration::from_mins(10), fired: BTreeMap::new(), steps: 0 };
    assert_eq!(show(p.due(&[b], t("2026-11-16T15:00:00Z")).0), vec![]);
}

const HEAD: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-TIMEZONE:Europe/Rome\r\n";

fn vevent(uid: &str, title: &str, start: &str, end: &str) -> String {
    format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{title}\r\nDTSTART:{start}\r\nDTEND:{end}\r\nEND:VEVENT\r\n"
    )
}

fn calendar(events: &[String]) -> String {
    format!("{HEAD}{}END:VCALENDAR\r\n", events.concat())
}

struct Run {
    rx: mpsc::Receiver<NewEvent>,
    stop: CancellationToken,
}

impl Run {
    fn start(path: &std::path::Path, home: &std::path::Path, now: &str) -> Run {
        let settings = CalendarSourceSettings {
            url: None,
            path: Some(path.to_str().unwrap().into()),
            refresh: Some(Duration::from_mins(15)),
            lead: Some(Duration::from_mins(10)),
        };
        let env = SourceEnv { home: home.into(), pulse_home: home.into() };
        let mut src = CalendarSource::new("uni", &settings, &env).unwrap();
        src.zone = Zone::Named(chrono_tz::Europe::Rome);
        src.clock = Arc::new(TokioClock::new(t(now)));
        let stop = CancellationToken::new();
        let (cx, rx, _h) = crate::context(src.source.clone(), 8, stop.clone(), Timestamp::EPOCH);
        tokio::spawn(Box::new(src).run(cx));
        Run { rx, stop }
    }

    /// (type, title, emitted at, minutes_until)
    async fn next(&mut self) -> (String, String, String, i64) {
        let e = self.rx.recv().await.unwrap();
        let short = e.type_.as_str().trim_start_matches("calendar.event.").to_owned();
        (
            short,
            e.data["title"].as_str().unwrap().into(),
            e.time.to_string(),
            e.data["minutes_until"].as_i64().unwrap(),
        )
    }
}

fn fire(kind: &str, title: &str, at: &str, minutes: i64) -> (String, String, String, i64) {
    (kind.into(), title.into(), at.into(), minutes)
}

#[tokio::test(start_paused = true)]
async fn a_file_calendar_fires_follows_edits_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uni.ics");
    std::fs::write(
        &path,
        calendar(&[
            vevent("a", "Lecture", "20261116T100000Z", "20261116T103000Z"),
            vevent("b", "Lab", "20261116T110000Z", "20261116T120000Z"),
            vevent("c", "Seminar", "20261116T120000Z", "20261116T123000Z"),
        ]),
    )
    .unwrap();
    let mut r = Run::start(&path, dir.path(), "2026-11-16T09:00:00Z");
    assert_eq!(r.next().await, fire("starting", "Lecture", "2026-11-16T09:50:00.000Z", 10));

    // Between polls: the lab moves to the afternoon, the seminar is removed, a meeting appears.
    std::fs::write(
        &path,
        calendar(&[
            vevent("a", "Lecture", "20261116T100000Z", "20261116T103000Z"),
            vevent("b", "Lab (moved)", "20261116T130000Z", "20261116T140000Z"),
            vevent("d", "Office hours", "20261116T104000Z", "20261116T105000Z"),
        ]),
    )
    .unwrap();
    let mut got = Vec::new();
    while got.last().map(|f: &(String, String, String, i64)| f.1.as_str()) != Some("Lab (moved)")
        || got.last().map(|f| f.0.as_str()) != Some("ended")
    {
        got.push(r.next().await);
    }
    assert_eq!(
        got,
        vec![
            fire("ended", "Lecture", "2026-11-16T10:30:00.000Z", -30),
            fire("starting", "Office hours", "2026-11-16T10:30:00.000Z", 10),
            fire("ended", "Office hours", "2026-11-16T10:50:00.000Z", -10),
            fire("starting", "Lab (moved)", "2026-11-16T12:50:00.000Z", 10),
            fire("ended", "Lab (moved)", "2026-11-16T14:00:00.000Z", -60),
        ]
    );
    r.stop.cancel();

    // Restarted five minutes later: nothing repeats (the lab's end is within the grace period,
    // but it already fired); the next thing is new.
    let mut events = std::fs::read_to_string(&path).unwrap().replace("END:VCALENDAR\r\n", "");
    events.push_str(&vevent("e", "Review", "20261116T142000Z", "20261116T143000Z"));
    events.push_str("END:VCALENDAR\r\n");
    std::fs::write(&path, events).unwrap();
    let mut r = Run::start(&path, dir.path(), "2026-11-16T14:05:00Z");
    assert_eq!(r.next().await, fire("starting", "Review", "2026-11-16T14:10:00.000Z", 10));
}

#[tokio::test(start_paused = true)]
async fn a_missing_file_is_reported_and_retried() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("later.ics");
    let settings = CalendarSourceSettings {
        url: None,
        path: Some(path.to_str().unwrap().into()),
        refresh: None,
        lead: None,
    };
    let env = SourceEnv { home: dir.path().into(), pulse_home: dir.path().into() };
    let mut src = CalendarSource::new("c", &settings, &env).unwrap();
    src.clock = Arc::new(TokioClock::new(t("2026-11-16T09:00:00Z")));
    src.zone = Zone::Named(chrono_tz::UTC);
    let (cx, mut rx, mut health) =
        crate::context(src.source.clone(), 8, CancellationToken::new(), Timestamp::EPOCH);
    tokio::spawn(Box::new(src).run(cx));
    health.wait_for(|h| h.state == pulse_contracts::status::SourceState::Degraded).await.unwrap();
    let message = health.borrow().message.clone().unwrap();
    assert!(message.contains("does not exist") && message.contains("retrying in 1m"), "{message}");
    std::fs::write(&path, calendar(&[vevent("x", "Soon", "20261116T091500Z", "20261116T093000Z")]))
        .unwrap();
    let e = rx.recv().await.unwrap();
    assert_eq!(e.data["title"], "Soon");
    assert_eq!(e.time, t("2026-11-16T09:05:00Z"));
}

// ---- HTTP ----

/// A tiny HTTP/1.1 server on port 0 serving one calendar with an ETag. Records the
/// `If-None-Match` each request carried.
struct Server {
    url: String,
    body: Arc<Mutex<(String, String)>>,
    seen: Arc<Mutex<Vec<Option<String>>>>,
}

impl Server {
    fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let body = Arc::new(Mutex::new((String::new(), "\"v1\"".to_owned())));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (b, s) = (body.clone(), seen.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let mut inm = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line
                        .strip_prefix("If-None-Match: ")
                        .or_else(|| line.strip_prefix("if-none-match: "))
                    {
                        inm = Some(v.trim().to_owned());
                    }
                }
                s.lock().unwrap().push(inm.clone());
                let (text, etag) = b.lock().unwrap().clone();
                let response = match path.as_str() {
                    "/error" => "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                    "/slow" => {
                        // Never answer; the client's timeout ends it.
                        let mut sink = Vec::new();
                        let _ = stream.read_to_end(&mut sink);
                        continue;
                    }
                    "/big" => {
                        let n = MAX_ICS_BYTES as usize + 1;
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n");
                        let chunk = vec![b'x'; 1 << 16];
                        let mut left = n;
                        while left > 0 {
                            let k = left.min(chunk.len());
                            if stream.write_all(&chunk[..k]).is_err() {
                                break;
                            }
                            left -= k;
                        }
                        continue;
                    }
                    _ if inm.as_deref() == Some(etag.as_str()) => {
                        format!("HTTP/1.1 304 Not Modified\r\nETag: {etag}\r\nConnection: close\r\n\r\n")
                    }
                    _ => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/calendar\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                        text.len()
                    ),
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Server { url, body, seen }
    }
}

#[test]
fn http_fetch_is_conditional_bounded_and_times_out() {
    let server = Server::start();
    *server.body.lock().unwrap() =
        (calendar(&[vevent("a", "A", "20261116T100000Z", "20261116T110000Z")]), "\"v1\"".into());
    let url = format!("{}/cal.ics", server.url);
    let timeout = std::time::Duration::from_secs(5);
    let Fetched::Body { text, etag, .. } =
        fetch_url(&url, None, None, timeout, &UrlPolicy::default()).unwrap()
    else {
        panic!()
    };
    assert!(text.contains("SUMMARY:A"));
    assert_eq!(etag.as_deref(), Some("\"v1\""));
    assert!(matches!(
        fetch_url(&url, etag.as_deref(), None, timeout, &UrlPolicy::default()).unwrap(),
        Fetched::Unchanged
    ));
    server.body.lock().unwrap().1 = "\"v2\"".into();
    assert!(matches!(
        fetch_url(&url, Some("\"v1\""), None, timeout, &UrlPolicy::default()).unwrap(),
        Fetched::Body { .. }
    ));
    assert_eq!(
        *server.seen.lock().unwrap(),
        vec![None, Some("\"v1\"".into()), Some("\"v1\"".into())]
    );

    let err =
        fetch_url(&format!("{}/error", server.url), None, None, timeout, &UrlPolicy::default())
            .unwrap_err();
    assert!(err.contains("HTTP 500"), "{err}");
    let err = fetch_url(&format!("{}/big", server.url), None, None, timeout, &UrlPolicy::default())
        .unwrap_err();
    assert!(err.contains("limit"), "{err}");
    let err = fetch_url(
        &format!("{}/slow", server.url),
        None,
        None,
        std::time::Duration::from_millis(300),
        &UrlPolicy::default(),
    )
    .unwrap_err();
    assert!(
        err.to_lowercase().contains("timeout") || err.to_lowercase().contains("timed out"),
        "{err}"
    );
}

#[tokio::test]
async fn a_url_calendar_fires_and_caches_for_conditional_requests() {
    let server = Server::start();
    let now = SystemClock.now();
    // Started a minute ago: within the grace period, so it fires on the first read.
    let fmt = |ts: Timestamp| {
        Timestamp::from_millis(ts.as_millis() / 1000 * 1000)
            .to_string()
            .replace(['-', ':'], "")
            .replace(".000", "")
    };
    let start = fmt(now.saturating_sub(Duration::from_mins(1)));
    let end = fmt(now.saturating_add(Duration::from_hours(1)));
    *server.body.lock().unwrap() = (calendar(&[vevent("n", "Now", &start, &end)]), "\"v1\"".into());
    let dir = tempfile::tempdir().unwrap();
    let settings = CalendarSourceSettings {
        url: Some(format!("{}/c.ics", server.url)),
        path: None,
        refresh: None,
        lead: None,
    };
    let env = SourceEnv { home: dir.path().into(), pulse_home: dir.path().into() };
    let src = CalendarSource::new("web", &settings, &env).unwrap();
    let stop = CancellationToken::new();
    let (cx, mut rx, _h) = crate::context(src.source.clone(), 8, stop.clone(), Timestamp::EPOCH);
    let task = tokio::spawn(Box::new(src).run(cx));
    let e = rx.recv().await.unwrap();
    assert_eq!(e.type_.as_str(), "calendar.event.starting");
    assert_eq!(e.data["minutes_until"], -1);
    stop.cancel();
    task.await.unwrap().unwrap();
    // A restart sends the validator and, on 304, uses the cached body (and does not repeat).
    let src = CalendarSource::new("web", &settings, &env).unwrap();
    let stop = CancellationToken::new();
    let (cx, mut rx, mut health) =
        crate::context(src.source.clone(), 8, stop.clone(), Timestamp::EPOCH);
    tokio::spawn(Box::new(src).run(cx));
    health.wait_for(|h| h.state == pulse_contracts::status::SourceState::Healthy).await.unwrap();
    assert_eq!(*server.seen.lock().unwrap(), vec![None, Some("\"v1\"".into())]);
    assert!(rx.try_recv().is_err());
    stop.cancel();
}

#[test]
fn absurd_lead_or_refresh_is_a_config_error() {
    let env = SourceEnv { home: "/h".into(), pulse_home: "/p".into() };
    let huge = Duration::from_millis(u64::MAX);
    for (refresh, lead) in [(Some(huge), None), (None, Some(huge))] {
        let settings =
            CalendarSourceSettings { url: None, path: Some("/c.ics".into()), refresh, lead };
        let err = CalendarSource::new("c", &settings, &env).unwrap_err();
        assert!(matches!(err, SourceError::Config(_)), "{err}");
    }
}

#[test]
fn a_file_is_read_through_its_limit_and_must_be_a_regular_file() {
    let err = read_file(std::path::Path::new("/dev/zero"), &Validators::default()).unwrap_err();
    assert!(err.contains("not a regular file"), "{err}");
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.ics");
    std::fs::write(&big, vec![b'x'; MAX_ICS_BYTES as usize + 1]).unwrap();
    let err = read_file(&big, &Validators::default()).unwrap_err();
    assert!(err.contains("larger than"), "{err}");
}

#[test]
fn planner_evicts_the_oldest_in_one_pass() {
    let mut p = Planner { lead: Duration::from_mins(10), fired: BTreeMap::new(), steps: 0 };
    let now = t("2026-11-16T09:00:00Z");
    let extra = 2_000;
    for i in 0..MAX_FIRED + extra {
        let end = Timestamp::from_millis(now.as_millis() + i as i64);
        p.fired.insert((format!("k{i}"), Kind::Starting), end);
    }
    let _ = p.due(&[], now);
    assert_eq!(p.fired.len(), MAX_FIRED);
    assert!(!p.fired.contains_key(&(format!("k{}", extra - 1), Kind::Starting)));
    assert!(p.fired.contains_key(&(format!("k{extra}"), Kind::Starting)));
    let visited = MAX_FIRED + extra;
    assert!(p.steps <= 4 * visited, "{} steps for {visited} entries", p.steps);
}

#[tokio::test(start_paused = true)]
async fn fires_reuse_the_expansion_and_a_budget_reached_shows_in_health() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uni.ics");
    let mut events: Vec<String> = (0..4)
        .map(|i| {
            let start = format!("20261116T{:02}0000Z", 10 + i);
            let end = format!("20261116T{:02}3000Z", 10 + i);
            vevent(&format!("e{i}"), &format!("Lecture {i}"), &start, &end)
        })
        .collect();
    events.push(
        "BEGIN:VEVENT\r\nUID:s\r\nSUMMARY:Every second\r\nDTSTART:19000101T000000Z\r\nRRULE:FREQ=SECONDLY\r\nEND:VEVENT\r\n".into(),
    );
    std::fs::write(&path, calendar(&events)).unwrap();
    let settings = CalendarSourceSettings {
        url: None,
        path: Some(path.to_str().unwrap().into()),
        refresh: Some(Duration::from_mins(15)),
        lead: Some(Duration::from_mins(10)),
    };
    let env = SourceEnv { home: dir.path().into(), pulse_home: dir.path().into() };
    let mut src = CalendarSource::new("uni", &settings, &env).unwrap();
    src.zone = Zone::Named(chrono_tz::Europe::Rome);
    src.clock = Arc::new(TokioClock::new(t("2026-11-16T09:00:00Z")));
    let expansions = src.expansions.clone();
    let (cx, mut rx, mut health) =
        crate::context(src.source.clone(), 8, CancellationToken::new(), Timestamp::EPOCH);
    tokio::spawn(Box::new(src).run(cx));
    let mut titles = Vec::new();
    while titles.len() < 8 {
        let e = rx.recv().await.unwrap();
        titles.push(format!("{} {}", e.type_.as_str(), e.data["title"].as_str().unwrap()));
    }
    assert_eq!(titles.last().map(String::as_str), Some("calendar.event.ended Lecture 3"));
    // Eight fires and twelve reads of the (unchanged) file over four hours: one expansion.
    assert_eq!(expansions.load(std::sync::atomic::Ordering::Relaxed), 1);
    let h = health.wait_for(|h| h.state == pulse_contracts::status::SourceState::Degraded).await;
    let message = h.unwrap().message.clone().unwrap();
    assert!(message.contains("\"Every second\" has more than 2000 occurrences"), "{message}");
}

/// A server on 127.0.0.1 that answers each request with `respond(path)`, until dropped.
fn serve(respond: impl Fn(&str) -> String + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            if reader.read_line(&mut first).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let _ = stream.write_all(respond(&path).as_bytes());
        }
    });
    url
}

fn redirect(to: &str) -> String {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// A "local service": a listener nobody should connect to. Whether anyone did is read without
/// waiting: a completed connection is in the backlog before `fetch_url` returns.
fn local_service() -> (TcpListener, u16) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}

fn was_reached(l: &TcpListener) -> bool {
    match l.accept() {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn a_feed_cannot_lead_the_daemon_to_a_local_address() {
    let timeout = std::time::Duration::from_secs(5);
    let cal = calendar(&[vevent("a", "A", "20261116T100000Z", "20261116T110000Z")]);
    let ok = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{cal}",
        cal.len()
    );
    // Every server here is on 127.0.0.1; the policy counts only the "local service"'s port as
    // local, so the feed stands for a server on the internet.
    let (service, port) = local_service();
    let policy = || UrlPolicy::with_local(move |a| a.port() == port);
    let feed = serve(move |path| match path {
        "/to-ip" => redirect(&format!("http://127.0.0.1:{port}/admin")),
        "/to-name" => redirect(&format!("http://localhost:{port}/admin")),
        "/loop" => redirect("/loop"),
        _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
    });
    for path in ["/to-ip", "/to-name"] {
        let err = fetch_url(&format!("{feed}{path}"), None, None, timeout, &policy()).unwrap_err();
        assert!(err.contains("local or private address"), "{path}: {err}");
        assert!(!was_reached(&service), "{path}: the redirect reached the local service");
    }
    let err = fetch_url(&format!("{feed}/loop"), None, None, timeout, &policy()).unwrap_err();
    assert!(err.to_lowercase().contains("redirect"), "{err}");

    // The configured host turning local later (DNS rebinding) is refused too.
    let rebound = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = rebound.clone();
    let body = ok.clone();
    let feed = serve(move |_| body.clone());
    let feed_port: u16 = feed.rsplit(':').next().unwrap().parse().unwrap();
    let policy = UrlPolicy::with_local(move |a| {
        a.port() == feed_port && flag.load(std::sync::atomic::Ordering::Relaxed)
    });
    let url = format!("{feed}/cal.ics");
    assert!(matches!(fetch_url(&url, None, None, timeout, &policy), Ok(Fetched::Body { .. })));
    rebound.store(true, std::sync::atomic::Ordering::Relaxed);
    let err = fetch_url(&url, None, None, timeout, &policy).unwrap_err();
    assert!(err.contains("now resolves to a local"), "{err}");

    // A calendar the user keeps on a local address may redirect locally.
    let body = ok.clone();
    let local_feed = serve(move |_| body.clone());
    let front = serve(move |_| redirect(&format!("{local_feed}/cal.ics")));
    let got = fetch_url(&format!("{front}/x"), None, None, timeout, &UrlPolicy::default());
    assert!(matches!(got, Ok(Fetched::Body { .. })), "{:?}", got.err());
}
