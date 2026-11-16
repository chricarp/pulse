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
    /// The page `pulse open` points the browser at; it redirects to a URL with a login code.
    pub fn login_page(&self) -> PathBuf {
        self.root.join("open.html")
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

    /// Writes [`Self::login_page`] (mode 0600), a page that sends the browser to `url`, so that
    /// a login code reaches the browser without appearing in any process's arguments, where
    /// other users could read it. Each `pulse open` replaces the page; the code in it works
    /// once and expires after two minutes.
    pub fn write_login_page(&self, url: &str) -> io::Result<PathBuf> {
        let attr: String = url
            .chars()
            .map(|c| match c {
                '&' => "&amp;".to_owned(),
                '"' => "&quot;".to_owned(),
                '<' => "&lt;".to_owned(),
                '>' => "&gt;".to_owned(),
                c => c.to_string(),
            })
            .collect();
        // `location.replace` keeps this file out of the tab's history; the meta refresh and the
        // link are for browsers that do not run scripts from files.
        let js = serde_json::to_string(url).map_err(io::Error::other)?.replace('<', "\\u003c");
        let page = format!(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\">\n\
             <meta http-equiv=\"refresh\" content=\"0;url={attr}\"><title>Opening Pulse</title>\n\
             <script>location.replace({js})</script></head>\n\
             <body><p>Opening <a href=\"{attr}\">Pulse</a>…</p></body></html>\n"
        );
        let path = self.login_page();
        write_atomic(&path, page.as_bytes(), 0o600)?;
        Ok(path)
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

/// Writes `bytes` to a temporary file next to `path`, flushes it, and renames it over `path`, so
/// readers see the old content or the new, never a mix.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
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
}
