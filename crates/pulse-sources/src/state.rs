//! Small per-source state files under `$PULSE_HOME/sources/`, so a restart neither repeats a
//! fire nor forgets one it missed. Missing or unreadable state is treated as "no state": the
//! worst outcome is the same as a first start.

use std::io::Write;
use std::path::PathBuf;

use pulse_contracts::SourceRef;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::SourceEnv;

#[derive(Clone, Debug)]
pub(crate) struct StateFile {
    path: PathBuf,
}

impl StateFile {
    pub(crate) fn new(env: &SourceEnv, source: &SourceRef) -> Self {
        // Source names may contain `/` (project prefixes); encode everything outside a small
        // safe set so the name maps to exactly one file.
        let mut name = String::new();
        for b in source.name.bytes() {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                name.push(char::from(b));
            } else {
                name.push_str(&format!("%{b:02X}"));
            }
        }
        StateFile {
            path: env.pulse_home.join("sources").join(format!("{}-{name}.json", source.kind)),
        }
    }

    pub(crate) fn load<T: DeserializeOwned>(&self) -> Option<T> {
        let bytes = std::fs::read(&self.path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Writes atomically (temp file, then rename), so a crash leaves the old state or the new.
    pub(crate) fn store<T: Serialize>(&self, value: &T) -> std::io::Result<()> {
        write_atomic(&self.path, &serde_json::to_vec(value).map_err(std::io::Error::other)?)
    }

    /// Another file for the same source, such as a cached download.
    pub(crate) fn sibling(&self, extension: &str) -> PathBuf {
        self.path.with_extension(extension)
    }
}

pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(path);
    std::fs::create_dir_all(dir)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    // The rename itself survives a power loss only once the directory is synced. A filesystem
    // that cannot sync directories (EINVAL) offers no more.
    match std::fs::File::open(dir).and_then(|d| d.sync_all()) {
        Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use pulse_contracts::SourceKind;

    use super::*;

    #[test]
    fn round_trips_and_encodes_names() {
        let dir = tempfile::tempdir().unwrap();
        let env = SourceEnv { home: dir.path().into(), pulse_home: dir.path().into() };
        let a = StateFile::new(&env, &SourceRef::new(SourceKind::Schedule, "proj/daily"));
        let b = StateFile::new(&env, &SourceRef::new(SourceKind::Schedule, "proj_daily"));
        assert_ne!(a.path, b.path);
        assert!(a.path.ends_with("sources/schedule-proj%2Fdaily.json"));
        assert_eq!(a.load::<u32>(), None);
        a.store(&7u32).unwrap();
        assert_eq!(a.load::<u32>(), Some(7));
        std::fs::write(&a.path, b"not json").unwrap();
        assert_eq!(a.load::<u32>(), None);
    }
}
