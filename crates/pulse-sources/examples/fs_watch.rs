//! Watches a directory with the fs source and prints every event and health change, then the
//! coalescer's peak footprint on Ctrl-C. For checking behaviour by hand on a real machine:
//!
//! ```sh
//! cargo run -p pulse-sources --example fs_watch -- /path/to/dir [quiet] [burst_threshold]
//! ```
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use pulse_contracts::config::{FsSourceSettings, SourceConfig, SourceSettings};
use pulse_contracts::{Duration, Timestamp};
use pulse_sources::fs::{FsFootprint, FsSource};
use pulse_sources::{CancellationToken, Source, SourceEnv};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: fs_watch <dir> [quiet] [burst_threshold]");
    let quiet = args.next().map(|q| Duration::parse(&q).expect("quiet like 2s"));
    let burst_threshold = args.next().map(|t| t.parse().expect("a number"));
    let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let settings =
        FsSourceSettings { path, ignore: vec![], recursive: true, quiet, burst_threshold };
    let config = SourceConfig {
        name: "manual".into(),
        enabled: true,
        settings: SourceSettings::Fs(settings.clone()),
    };
    let env = SourceEnv {
        home: home.clone(),
        pulse_home: std::env::temp_dir().join("pulse-fs-watch-example"),
    };
    let probe = Arc::new(Mutex::new(FsFootprint::default()));
    let src =
        FsSource::new(&config.name, &settings, &env).unwrap().with_footprint_probe(probe.clone());
    let stop = CancellationToken::new();
    let (cx, mut rx, mut health) =
        pulse_sources::context(src.source_ref().clone(), 64, stop.clone(), Timestamp::EPOCH);
    let task = tokio::spawn(Box::new(src).run(cx));
    let started = std::time::Instant::now();
    loop {
        tokio::select! {
            e = rx.recv() => {
                let Some(e) = e else { break };
                println!("{:>8.3}s {} {} coalesced={:?} {}", started.elapsed().as_secs_f64(), e.type_, e.subject.unwrap_or_default(), e.coalesced, e.data);
            }
            r = health.changed() => {
                if r.is_err() { break; }
                let h = health.borrow().clone();
                println!("{:>8.3}s health {:?} {}", started.elapsed().as_secs_f64(), h.state, h.message.unwrap_or_default());
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    stop.cancel();
    let _ = task.await;
    println!("peak footprint: {:?}", *probe.lock().unwrap());
}
