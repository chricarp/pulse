//! The Pulse home: paths, the single-instance lock, the daemon secret and `daemon.json`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use pulse_contracts::status::DaemonInfo;
use rustix::fs::{FlockOperation, flock};

/// Paths inside `$PULSE_HOME`.
#[derive(Clone, Debug)]
pub struct PulseHome {
    root: PathBuf,
}

impl PulseHome {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        PulseHome { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config_file(&self) -> PathBuf {
        self.root.join("pulse.toml")
    }
    pub fn database(&self) -> PathBuf {
        self.root.join("pulse.db")
    }
    pub fn daemon_info(&self) -> PathBuf {
        self.root.join("daemon.json")
    }
    pub fn token_file(&self) -> PathBuf {
        self.root.join("token")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.root.join("daemon.lock")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }
    pub fn run_dir(&self, run: pulse_contracts::RunId) -> PathBuf {
        self.runs_dir().join(run.to_string())
    }

    /// Creates the home (mode 0700: it holds the secret and the event log) and its subdirectories.
    pub fn create_dirs(&self) -> io::Result<()> {
        if !self.root.exists() {
            fs::create_dir_all(&self.root)?;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        }
        fs::create_dir_all(self.logs_dir())?;
        fs::create_dir_all(self.runs_dir())?;
        Ok(())
    }

    /// Reads `daemon.json`, if a daemon has written one.
    pub fn read_daemon_info(&self) -> io::Result<Option<DaemonInfo>> {
        match fs::read(self.daemon_info()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn write_daemon_info(&self, info: &DaemonInfo) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(info).map_err(io::Error::other)?;
        write_atomic(&self.daemon_info(), &json, 0o644)
    }

    pub(crate) fn remove_daemon_info(&self) {
        let _ = fs::remove_file(self.daemon_info());
    }

    /// Reads the daemon secret. Clients use it as the bearer token.
    pub fn read_token(&self) -> io::Result<String> {
        Ok(fs::read_to_string(self.token_file())?.trim().to_owned())
    }

    /// Reads the secret, creating it (mode 0600) on first start. A secret file readable by others
    /// is tightened, because the token grants full control.
    pub(crate) fn load_or_create_token(&self) -> io::Result<String> {
        let path = self.token_file();
        match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(mut f) => {
                let mut bytes = [0u8; 32];
                getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
                let token = crate::auth::hex(&bytes);
                f.write_all(token.as_bytes())?;
                f.sync_all()?;
                Ok(token)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let mode = fs::metadata(&path)?.permissions().mode();
                if mode & 0o077 != 0 {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
                }
                let token = self.read_token()?;
                if token.len() < 32 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} is too short to be a Pulse token", path.display()),
                    ));
                }
                Ok(token)
            }
            Err(e) => Err(e),
        }
    }
}

/// Held for the daemon's lifetime: one daemon per Pulse home. The OS releases the lock when the
/// process exits, however it exits.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// `Ok(None)` when another process holds the lock.
    pub fn try_acquire(home: &PulseHome) -> io::Result<Option<InstanceLock>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(home.lock_file())?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(InstanceLock { _file: file })),
            Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Blocks until no daemon holds the lock (it has exited), then releases it again. For
    /// `pulse stop`; call it from a blocking thread.
    pub fn wait_released(home: &PulseHome) -> io::Result<()> {
        let file = match OpenOptions::new().read(true).open(home.lock_file()) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        flock(&file, FlockOperation::LockShared)?;
        Ok(())
    }
}

/// Follows `path` while it is a symbolic link, to the file it finally names (which need not
/// exist). Directories along the way are left as they are.
pub(crate) fn resolve_links(path: &Path) -> io::Result<PathBuf> {
    let mut path = path.to_path_buf();
    // Linux's own limit; a cycle ends here.
    for _ in 0..40 {
        match fs::read_link(&path) {
            Ok(target) => {
                // A relative target is relative to the link's directory; `join` keeps an
                // absolute one as it is.
                path = path.parent().unwrap_or(Path::new(".")).join(target);
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::InvalidInput | io::ErrorKind::NotFound) => {
                return Ok(path);
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(format!("{}: too many levels of symbolic links", path.display())))
}

/// Writes `bytes` to a temporary file next to `path`, flushes it, and renames it over `path`, so
/// readers see the old content or the new, never a mix. When `path` is a symbolic link (a
/// `pulse.toml` kept in a dotfiles repository), the file it points to is replaced and the link
/// stays: renaming over the link would turn it into a regular file, and later edits to the
/// linked copy would be silently ignored.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let path = &resolve_links(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let mut nonce = [0u8; 6];
    getrandom::fill(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
    let tmp = dir.join(format!(".{name}.{}.tmp", crate::auth::hex(&nonce)));
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_private_and_stable_and_lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let home = PulseHome::new(dir.path().join("home"));
        home.create_dirs().unwrap();
        let token = home.load_or_create_token().unwrap();
        assert_eq!(token.len(), 64);
        let mode = fs::metadata(home.token_file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::set_permissions(home.token_file(), fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(home.load_or_create_token().unwrap(), token);
        let mode = fs::metadata(home.token_file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "loosened permissions are tightened");

        let lock = InstanceLock::try_acquire(&home).unwrap();
        assert!(lock.is_some());
        assert!(InstanceLock::try_acquire(&home).unwrap().is_none());
        drop(lock);
        assert!(InstanceLock::try_acquire(&home).unwrap().is_some());
    }

    #[test]
    fn writing_through_a_symlink_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let dotfiles = dir.path().join("dotfiles");
        let home = dir.path().join("home");
        fs::create_dir_all(&dotfiles).unwrap();
        fs::create_dir_all(&home).unwrap();
        let real = dotfiles.join("pulse.toml");
        fs::write(&real, "old").unwrap();
        // A relative link, then a link to that link, then a dangling one.
        let link = home.join("pulse.toml");
        std::os::unix::fs::symlink("../dotfiles/pulse.toml", &link).unwrap();
        let chained = home.join("chained.toml");
        std::os::unix::fs::symlink(&link, &chained).unwrap();
        let dangling = home.join("dangling.toml");
        std::os::unix::fs::symlink("../dotfiles/new.toml", &dangling).unwrap();

        write_atomic(&link, b"new", 0o640).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "still a link");
        assert_eq!(fs::read_to_string(&real).unwrap(), "new", "the target was written");
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o640);

        write_atomic(&chained, b"newer", 0o600).unwrap();
        assert!(fs::symlink_metadata(&chained).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "newer");

        write_atomic(&dangling, b"created", 0o600).unwrap();
        assert!(fs::symlink_metadata(&dangling).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(dotfiles.join("new.toml")).unwrap(), "created");

        // No temporary file is left next to the link.
        let names: Vec<_> = fs::read_dir(&home).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
    }
}
