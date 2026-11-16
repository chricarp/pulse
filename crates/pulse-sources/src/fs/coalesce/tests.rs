use std::path::Path;

use super::*;

const QUIET: i64 = 2_000;

fn ts(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn coalescer(threshold: usize) -> Coalescer {
    let ig = Ignore::new("/w", &[], Path::new("/home/me")).unwrap();
    Coalescer::new("/w", ig, Settings::new(Duration::from_millis(QUIET as u64), threshold))
}

fn c(p: &str) -> Signal {
    Signal::Created(p.into())
}
fn m(p: &str) -> Signal {
    Signal::Modified(p.into())
}
fn r(p: &str) -> Signal {
    Signal::Removed(p.into())
}
fn from(p: &str, cookie: u64) -> Signal {
    Signal::MovedFrom { path: p.into(), cookie: Some(cookie), dir: false }
}
fn to(p: &str, cookie: u64) -> Signal {
    Signal::MovedTo { path: p.into(), cookie: Some(cookie), dir: false }
}
fn ch(p: &str, exists: bool) -> Signal {
    Signal::Changed { path: p.into(), exists }
}

/// Drives the coalescer the way the async loop does: before each signal, settle everything due
/// by its time; after the script, keep firing the timer until `end`. Returns each change with
/// the time it was emitted.
fn run(co: &mut Coalescer, script: Vec<(i64, Signal)>, end: i64) -> Vec<(i64, Change)> {
    let mut out = Vec::new();
    let mut next: Option<Timestamp> = None;
    let fire = |co: &mut Coalescer, upto: i64, next: &mut Option<Timestamp>, out: &mut Vec<_>| {
        while let Some(at) = *next {
            if at.as_millis() > upto {
                break;
            }
            let (settled, n) = co.poll(at, usize::MAX);
            out.extend(settled.into_iter().map(|s| (at.as_millis(), s.change)));
            *next = n;
        }
    };
    for (t, sig) in script {
        fire(co, t, &mut next, &mut out);
        co.push(sig, ts(t));
        let (settled, n) = co.poll(ts(t), usize::MAX);
        out.extend(settled.into_iter().map(|s| (t, s.change)));
        next = n;
    }
    fire(co, end, &mut next, &mut out);
    out
}

fn changes(co: &mut Coalescer, script: Vec<(i64, Signal)>) -> Vec<Change> {
    run(co, script, i64::MAX / 4).into_iter().map(|(_, ch)| ch).collect()
}

fn created(p: &str) -> Change {
    Change::Created(p.into())
}
fn modified(p: &str) -> Change {
    Change::Modified(p.into())
}
fn deleted(p: &str) -> Change {
    Change::Deleted(p.into())
}
fn moved(a: &str, b: &str) -> Change {
    Change::Moved { from: a.into(), to: b.into() }
}

#[test]
fn per_path_net_effects() {
    // (name, files that existed before, script, expected changes)
    type Case<'a> = (&'a str, Vec<&'a str>, Vec<(i64, Signal)>, Vec<Change>);
    let cases: Vec<Case> = vec![
        (
            "a file written for several seconds is one creation",
            vec![],
            (0..=50)
                .map(|i| (i * 100, if i == 0 { c("/w/a.pdf") } else { m("/w/a.pdf") }))
                .collect(),
            vec![created("/w/a.pdf")],
        ),
        (
            "forty saves are one modification",
            vec!["/w/notes.md"],
            (0..40).map(|i| (i * 300, m("/w/notes.md"))).collect(),
            vec![modified("/w/notes.md")],
        ),
        (
            "created then deleted is nothing",
            vec![],
            vec![(0, c("/w/t")), (500, m("/w/t")), (900, r("/w/t"))],
            vec![],
        ),
        ("deleted", vec!["/w/old"], vec![(0, r("/w/old"))], vec![deleted("/w/old")]),
        (
            "deleted then recreated is a modification",
            vec!["/w/x"],
            vec![(0, r("/w/x")), (10, c("/w/x"))],
            vec![modified("/w/x")],
        ),
        (
            "sed -i style: temp file renamed over the target",
            vec!["/w/a.md"],
            vec![
                (0, c("/w/sedAb12Z")),
                (1, m("/w/sedAb12Z")),
                (2, from("/w/sedAb12Z", 7)),
                (2, to("/w/a.md", 7)),
            ],
            vec![modified("/w/a.md")],
        ),
        (
            "the same pattern onto a new name is a creation",
            vec![],
            vec![
                (0, c("/w/out.tmp1")),
                (1, m("/w/out.tmp1")),
                (2, from("/w/out.tmp1", 7)),
                (2, to("/w/out.json", 7)),
            ],
            vec![created("/w/out.json")],
        ),
        (
            "vim: original renamed to a backup, new file written, backup removed",
            vec!["/w/a.md"],
            vec![
                (0, c("/w/4913")),
                (0, r("/w/4913")),
                (1, from("/w/a.md", 3)),
                (1, to("/w/a.md~", 3)),
                (2, c("/w/a.md")),
                (3, m("/w/a.md")),
                (4, r("/w/a.md~")),
            ],
            vec![modified("/w/a.md")],
        ),
        (
            "JetBrains safe write",
            vec!["/w/Main.kt"],
            vec![
                (0, c("/w/Main.kt___jb_tmp___")),
                (1, m("/w/Main.kt___jb_tmp___")),
                (2, from("/w/Main.kt", 1)),
                (2, to("/w/Main.kt___jb_old___", 1)),
                (3, from("/w/Main.kt___jb_tmp___", 2)),
                (3, to("/w/Main.kt", 2)),
                (4, r("/w/Main.kt___jb_old___")),
            ],
            vec![modified("/w/Main.kt")],
        ),
        (
            "Chrome download: .crdownload renamed into place",
            vec![],
            {
                let mut s = vec![(0, c("/w/x.pdf.crdownload"))];
                s.extend((1..100).map(|i| (i * 50, m("/w/x.pdf.crdownload"))));
                s.extend([(5_000, from("/w/x.pdf.crdownload", 9)), (5_000, to("/w/x.pdf", 9))]);
                s
            },
            vec![created("/w/x.pdf")],
        ),
        (
            "Firefox download: empty placeholder held while the .part file exists",
            vec![],
            {
                let mut s = vec![(0, c("/w/x.pdf")), (5, c("/w/x.pdf.part"))];
                // Slow, with gaps longer than the quiet period.
                s.extend((1..10).map(|i| (i * 5_000, m("/w/x.pdf.part"))));
                s.extend([(60_000, from("/w/x.pdf.part", 4)), (60_000, to("/w/x.pdf", 4))]);
                s
            },
            vec![created("/w/x.pdf")],
        ),
        (
            "rename",
            vec!["/w/a"],
            vec![(0, from("/w/a", 1)), (0, to("/w/b", 1))],
            vec![moved("/w/a", "/w/b")],
        ),
        (
            "rename then edit is still a move",
            vec!["/w/a"],
            vec![(0, from("/w/a", 1)), (0, to("/w/b", 1)), (100, m("/w/b"))],
            vec![moved("/w/a", "/w/b")],
        ),
        (
            "renamed twice",
            vec!["/w/a"],
            vec![
                (0, from("/w/a", 1)),
                (0, to("/w/b", 1)),
                (10, from("/w/b", 2)),
                (10, to("/w/c", 2)),
            ],
            vec![moved("/w/a", "/w/c")],
        ),
        (
            "renamed and back",
            vec!["/w/a"],
            vec![
                (0, from("/w/a", 1)),
                (0, to("/w/b", 1)),
                (10, from("/w/b", 2)),
                (10, to("/w/a", 2)),
            ],
            vec![],
        ),
        (
            "renamed then deleted",
            vec!["/w/a"],
            vec![(0, from("/w/a", 1)), (0, to("/w/b", 1)), (10, r("/w/b"))],
            vec![deleted("/w/a")],
        ),
        ("moved out of the tree", vec!["/w/a"], vec![(0, from("/w/a", 1))], vec![deleted("/w/a")]),
        ("moved into the tree", vec![], vec![(0, to("/w/a", 1))], vec![created("/w/a")]),
        (
            "moved into the tree over a file",
            vec!["/w/a"],
            vec![(0, to("/w/a", 1))],
            vec![modified("/w/a")],
        ),
        (
            "a half whose path is reused resolves first",
            vec!["/w/a"],
            vec![(0, from("/w/a", 1)), (1, c("/w/a"))],
            vec![modified("/w/a")],
        ),
        (
            "moved to an ignored name",
            vec!["/w/a"],
            vec![(0, from("/w/a", 1)), (0, to("/w/node_modules/a", 1))],
            vec![deleted("/w/a")],
        ),
        (
            "ignored paths",
            vec![],
            vec![(0, c("/w/.git/index")), (0, c("/w/.a.swp")), (0, m("/w/.DS_Store"))],
            vec![],
        ),
        // FSEvents: coarse, merged reports; the index says what existed.
        (
            "coarse: new file",
            vec![],
            vec![(0, ch("/w/n", true)), (5, ch("/w/n", true))],
            vec![created("/w/n")],
        ),
        (
            "coarse: known file",
            vec!["/w/k"],
            vec![(0, ch("/w/k", true)), (5, ch("/w/k", true))],
            vec![modified("/w/k")],
        ),
        (
            "coarse: known file gone",
            vec!["/w/k"],
            vec![(0, ch("/w/k", false))],
            vec![deleted("/w/k")],
        ),
        (
            "coarse: created and removed inside one batch",
            vec![],
            vec![(0, ch("/w/t", false))],
            vec![],
        ),
        (
            "coarse: rename halves without cookies pair by adjacency",
            vec!["/w/a"],
            vec![
                (0, Signal::MovedFrom { path: "/w/a".into(), cookie: None, dir: false }),
                (0, Signal::MovedTo { path: "/w/b".into(), cookie: None, dir: false }),
            ],
            vec![moved("/w/a", "/w/b")],
        ),
        (
            "coarse: halves in the other order",
            vec!["/w/a"],
            vec![
                (0, Signal::MovedTo { path: "/w/b".into(), cookie: None, dir: false }),
                (0, Signal::MovedFrom { path: "/w/a".into(), cookie: None, dir: false }),
            ],
            vec![moved("/w/a", "/w/b")],
        ),
        (
            "scans: unknown files are new, known files only when recently written",
            vec!["/w/k", "/w/k2"],
            vec![
                (0, Signal::Scanned { path: "/w/n".into(), recent: false }),
                (0, Signal::Scanned { path: "/w/k".into(), recent: false }),
                (0, Signal::Scanned { path: "/w/k2".into(), recent: true }),
            ],
            vec![modified("/w/k2"), created("/w/n")],
        ),
        (
            "a create report racing a scan is one creation",
            vec![],
            vec![(0, c("/w/d/f")), (1, Signal::Scanned { path: "/w/d/f".into(), recent: false })],
            vec![created("/w/d/f")],
        ),
    ];
    for (name, existing, script, mut want) in cases {
        let mut co = coalescer(200);
        for p in existing {
            co.note_existing(p);
        }
        let mut got = changes(&mut co, script);
        let key = |c: &Change| format!("{c:?}");
        got.sort_by_key(key);
        want.sort_by_key(key);
        assert_eq!(got, want, "{name}");
        assert_eq!(co.footprint().pending, 0, "{name}: nothing left pending");
    }
}

#[test]
fn settles_exactly_one_quiet_period_after_the_last_signal() {
    let mut co = coalescer(200);
    let script = vec![(0, c("/w/a")), (1_500, m("/w/a")), (3_000, m("/w/a"))];
    let got = run(&mut co, script, 10_000);
    assert_eq!(got, vec![(3_000 + QUIET, created("/w/a"))]);
    // Downloads are held, but not forever.
    let mut co = coalescer(200);
    let got = run(&mut co, vec![(0, c("/w/x.pdf")), (1, c("/w/x.pdf.part"))], 3_600_000);
    assert_eq!(got, vec![(300_000, created("/w/x.pdf"))]);
}

#[test]
fn a_file_that_never_goes_quiet_is_reported_after_max_hold() {
    let mut co = coalescer(200);
    co.note_existing("/w/app.log");
    let script = (0..700).map(|i| (i * 1_000, m("/w/app.log"))).collect();
    let got = run(&mut co, script, 702_000);
    let times: Vec<i64> = got.iter().map(|(t, _)| *t).collect();
    assert_eq!(times, vec![300_000, 600_000, 699_000 + QUIET]);
    assert!(got.iter().all(|(_, ch)| *ch == modified("/w/app.log")));
}

#[test]
fn directory_renames_become_scan_actions() {
    let mut co = coalescer(200);
    co.push(Signal::MovedFrom { path: "/w/a".into(), cookie: Some(5), dir: true }, ts(0));
    co.push(Signal::MovedTo { path: "/w/b".into(), cookie: Some(5), dir: true }, ts(0));
    co.push(Signal::MovedTo { path: "/w/c".into(), cookie: Some(6), dir: true }, ts(0));
    co.push(Signal::MovedFrom { path: "/w/d".into(), cookie: Some(7), dir: true }, ts(0));
    co.push(Signal::MovedTo { path: "/w/node_modules".into(), cookie: Some(7), dir: true }, ts(0));
    let _ = co.poll(ts(10_000), usize::MAX);
    assert_eq!(
        co.take_actions(),
        vec![
            Action::ScanMoved { from: "/w/a".into(), to: "/w/b".into() },
            Action::ScanAppeared("/w/c".into()),
        ]
    );
    // The scan of the moved directory reports its files as moved.
    co.note_existing("/w/a/x.pdf");
    let got = changes(
        &mut co,
        vec![(20_000, Signal::Moved { from: "/w/a/x.pdf".into(), to: "/w/b/x.pdf".into() })],
    );
    assert_eq!(got, vec![moved("/w/a/x.pdf", "/w/b/x.pdf")]);
}

fn burst(got: &[Change]) -> Vec<&Burst> {
    got.iter().filter_map(|c| if let Change::Burst(b) = c { Some(b) } else { None }).collect()
}

#[test]
fn a_storm_folds_into_one_burst_in_bounded_memory_and_spares_unrelated_edits() {
    let mut co = coalescer(200);
    co.note_existing("/w/notes.md");
    co.note_existing("/w/big/keep.txt");
    let mut script: Vec<(i64, Signal)> = Vec::new();
    // 10,000 files in 100 directories, each created and written, over ~5 seconds.
    for i in 0..10_000i64 {
        let p = format!("/w/big/d{:03}/f{i:05}.bin", i / 100);
        script.push((i / 2, c(&p)));
        script.push((i / 2, m(&p)));
    }
    // An unrelated edit in the middle of the storm, outside its subtree.
    script.insert(5_000, (1_250, m("/w/notes.md")));
    // And one inside it, which is folded.
    script.push((4_999, m("/w/big/keep.txt")));
    let mut peak = Footprint::default();
    let mut out = Vec::new();
    for (t, s) in script {
        co.push(s, ts(t));
        let (settled, _) = co.poll(ts(t), usize::MAX);
        out.extend(settled.into_iter().map(|s| s.change));
        peak = peak.max(co.footprint());
    }
    let (settled, _) = co.poll(ts(1_000_000), usize::MAX);
    out.extend(settled.into_iter().map(|s| s.change));
    assert_eq!(co.footprint().pending, 0);
    assert_eq!(co.footprint().storms, 0);

    assert!(out.contains(&modified("/w/notes.md")), "{out:?}");
    let bursts = burst(&out);
    assert_eq!(bursts.len(), 1, "{out:?}");
    let b = bursts[0];
    assert_eq!(b.dir, "/w/big");
    assert_eq!((b.created, b.modified, b.deleted), (10_000, 1, 0));
    assert_eq!(b.sample.len(), 8);
    assert!(b.sample.iter().all(|p| p.starts_with("/w/big/")));
    // Before the storm was recognised, at most a few thresholds' worth of paths were pending at
    // once, and nothing else was emitted.
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(peak.pending <= 400, "{peak:?}");
    assert!(peak.storms <= 16, "{peak:?}");
}

#[test]
fn storms_spread_over_subdirectories_fold_at_their_common_parent() {
    let mut co = coalescer(50);
    let mut script = Vec::new();
    for i in 0..500i64 {
        script.push((
            i,
            c(&format!("/w/data/{}/x/{i}.csv", ["a", "b", "c", "d", "e"][(i % 5) as usize])),
        ));
    }
    script.push((250, c("/w/report.txt")));
    script.sort_by_key(|(t, _)| *t);
    let out = changes(&mut co, script);
    let bursts = burst(&out);
    assert_eq!(bursts.len(), 1, "{out:?}");
    assert_eq!(bursts[0].dir, "/w/data");
    assert_eq!(bursts[0].created, 500);
    assert!(out.contains(&created("/w/report.txt")));
    assert_eq!(out.len(), 2);
}

#[test]
fn a_storm_of_temp_files_with_no_net_effect_emits_nothing() {
    let mut co = coalescer(50);
    let mut script = Vec::new();
    for i in 0..300i64 {
        let p = format!("/w/build/{i}.o");
        script.push((i, c(&p)));
        script.push((i, r(&p)));
    }
    assert_eq!(changes(&mut co, script), vec![]);
}

#[test]
fn past_the_exact_budget_storms_count_signals_without_growing() {
    let ig = Ignore::new("/w", &[], Path::new("/h")).unwrap();
    let mut s = Settings::new(Duration::from_millis(2_000), 50);
    s.storm_exact_capacity = 100;
    s.index_capacity = 1_000;
    let mut co = Coalescer::new("/w", ig, s);
    let mut peak = Footprint::default();
    for i in 0..5_000i64 {
        co.push(c(&format!("/w/s/f{i}")), ts(i / 10));
        peak = peak.max(co.footprint());
    }
    assert!(peak.storm_paths <= 100 && peak.index <= 1_000 && peak.pending <= 100, "{peak:?}");
    let (settled, _) = co.poll(ts(1_000_000), usize::MAX);
    let [Settled { change: Change::Burst(b), .. }] = &settled[..] else { panic!("{settled:?}") };
    assert_eq!(b.created, 5_000);
}

#[test]
fn a_busy_storm_reports_every_max_hold_and_keeps_folding() {
    let mut co = coalescer(10);
    let script = (0..700_000i64).step_by(100).map(|t| (t, c(&format!("/w/logs/{t}")))).collect();
    let got = run(&mut co, script, 710_000);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got.iter().all(|(_, ch)| matches!(ch, Change::Burst(b) if b.dir == "/w/logs")));
    let total: u64 =
        got.iter().map(|(_, ch)| if let Change::Burst(b) = ch { b.created } else { 0 }).sum();
    assert_eq!(total, 7_000);
}

#[test]
fn poll_respects_the_limit_and_asks_to_be_called_again() {
    let mut co = coalescer(200);
    for i in 0..5 {
        co.push(c(&format!("/w/{i}")), ts(0));
    }
    let (a, next) = co.poll(ts(QUIET), 2);
    assert_eq!((a.len(), next), (2, Some(ts(QUIET))));
    let (b, next) = co.poll(ts(QUIET), 10);
    assert_eq!((b.len(), next), (3, None));
}

#[test]
fn storm_roots() {
    // (pending paths relative to the root, storms, threshold) -> chosen roots
    type Case<'a> = (Vec<String>, Vec<&'a str>, usize, Vec<&'a str>);
    let cases: Vec<Case> = vec![
        // One dominant child, below the threshold itself: nothing yet.
        (
            (0..199)
                .map(|i| format!("dataset/p{}/{i}", i % 2))
                .chain(["notes.md".into()])
                .collect(),
            vec![],
            200,
            vec![],
        ),
        // Spread over subdirectories: their parent.
        ((0..50).map(|i| format!("data/{}/{i}", i % 5)).collect(), vec![], 50, vec!["data"]),
        // Flat in the root.
        ((0..50).map(|i| format!("{i}")).collect(), vec![], 50, vec![""]),
        // A deep dominant chain.
        (
            (0..60).map(|i| format!("a/b/c/{i}")).chain(["x".into()]).collect(),
            vec![],
            50,
            vec!["a/b/c"],
        ),
        // An existing storm and fresh activity next to it under the same parent.
        ((0..50).map(|i| format!("p/new/{i}")).collect(), vec!["p/old"], 50, vec!["p"]),
        // Items that sort between a child's items are not counted in that child.
        (
            (0..60).map(|i| format!("a/{i}")).chain((0..5).map(|i| format!("a.{i}"))).collect(),
            vec![],
            50,
            vec!["a"],
        ),
    ];
    for (paths, storms, t, want) in cases {
        let mut items: Vec<(&str, bool)> = paths
            .iter()
            .map(|p| (p.as_str(), false))
            .chain(storms.iter().map(|s| (*s, true)))
            .collect();
        items.sort_unstable();
        let mut out = Vec::new();
        plan_dir("", &items, t, &mut out);
        assert_eq!(out, want, "{paths:?}");
    }
}
