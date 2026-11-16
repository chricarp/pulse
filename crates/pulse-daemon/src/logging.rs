//! Daemon logs: `$PULSE_HOME/logs/pulse.log`, rotated by size (`pulse.log.1` … `.N`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use pulse_contracts::config::LogLevel;
use tracing_subscriber::fmt::writer::MakeWriterExt;

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const KEEP: u32 = 5;

/// A log file that rotates when it reaches [`MAX_BYTES`], keeping [`KEEP`] old files.
#[derive(Debug)]
pub struct RollingFile {
    path: PathBuf,
    file: File,
    size: u64,
}

impl RollingFile {
    pub fn open(dir: &Path) -> io::Result<RollingFile> {
        fs::create_dir_all(dir)?;
        let path = dir.join("pulse.log");
        let file = OpenOptions::new().create(true).append(true).mode(0o600).open(&path)?;
        let size = file.metadata()?.len();
        Ok(RollingFile { path, file, size })
    }

    fn rotate(&mut self) -> io::Result<()> {
        for n in (1..KEEP).rev() {
            let from = self.path.with_extension(format!("log.{n}"));
            if from.exists() {
                fs::rename(&from, self.path.with_extension(format!("log.{}", n + 1)))?;
            }
        }
        fs::rename(&self.path, self.path.with_extension("log.1"))?;
        self.file = OpenOptions::new().create(true).append(true).mode(0o600).open(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

impl Write for RollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size + buf.len() as u64 > MAX_BYTES && self.size > 0 {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.size += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn level(l: LogLevel) -> tracing::Level {
    match l {
        LogLevel::Error => tracing::Level::ERROR,
        LogLevel::Warn => tracing::Level::WARN,
        LogLevel::Info => tracing::Level::INFO,
        LogLevel::Debug => tracing::Level::DEBUG,
        LogLevel::Trace => tracing::Level::TRACE,
    }
}

/// Installs the global logger: the rotating file, and stderr too when `to_stderr`. Call once,
/// from the binary. The level comes from `[daemon] log_level` at startup.
pub fn init(home: &crate::PulseHome, log_level: LogLevel, to_stderr: bool) -> io::Result<()> {
    let file = Mutex::new(RollingFile::open(&home.logs_dir())?);
    let max = level(log_level);
    let builder = tracing_subscriber::fmt().with_ansi(false).with_target(false).with_max_level(max);
    let result = if to_stderr {
        builder.with_writer(file.and(io::stderr)).try_init()
    } else {
        builder.with_writer(file).try_init()
    };
    result.map_err(|e| io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_by_size() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RollingFile::open(dir.path()).unwrap();
        let line = vec![b'x'; (MAX_BYTES / 2) as usize + 1];
        for _ in 0..(KEEP + 3) {
            log.write_all(&line).unwrap();
        }
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names.len(), KEEP as usize + 1, "{names:?}");
        assert!(names.contains(&"pulse.log".to_owned()));
        assert!(names.contains(&format!("pulse.log.{KEEP}")));
    }
}
