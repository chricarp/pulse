//! The Pulse home: paths, the single-instance lock, the daemon secret and `daemon.json`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
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

    /// Creates the home and its subdirectories, and makes them private (0700) on every start,
    /// not only when creating them: the home holds the secret and the event log (paths, commit
    /// messages, prompts, output), and one that already existed (made by hand, by an older Pulse,
    /// or by `logs/` being opened first) kept whatever mode it had. Only the home and its own
    /// subdirectories are changed: a home reached through a symbolic link is the directory it
    /// points to, but a subdirectory that is a link to somewhere else is left alone, and so are
    /// the home's parents. A home owned by another user is refused; it cannot be made private.
    /// The database files are the store's (`pulse_store::Store::open`).
    pub fn create_dirs(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        make_private(&self.root, true)?;
        for dir in [self.logs_dir(), self.runs_dir(), self.root.join("sources")] {
            match fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => make_private(&dir, false)?,
                Err(e) => return Err(e),
            }
        }
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
        write_atomic(&self.daemon_info(), &json, 0o600)
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

/// Mode 0700 for a directory this user owns. `follow`: whether a symbolic link at `dir` is
/// followed (the home itself) or left as it is (a subdirectory linked elsewhere).
fn make_private(dir: &Path, follow: bool) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = if follow { fs::metadata(dir)? } else { fs::symlink_metadata(dir)? };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", dir.display()),
        ));
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} belongs to another user; Pulse keeps its data private", dir.display()),
        ));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
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
    fn an_existing_home_is_made_private_and_links_out_of_it_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let open = |p: &Path, m| fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
        // A home that already existed, as a group-writable directory reached through a link,
        // with loose subdirectories and a log directory that is a link to somewhere else.
        let real = dir.path().join("real-home");
        let elsewhere = dir.path().join("elsewhere");
        for d in [&real, &real.join("runs"), &real.join("sources"), &elsewhere] {
            fs::create_dir_all(d).unwrap();
            open(d, 0o775);
        }
        std::os::unix::fs::symlink(&elsewhere, real.join("logs")).unwrap();
        let root = dir.path().join("home");
        std::os::unix::fs::symlink(&real, &root).unwrap();

        PulseHome::new(&root).create_dirs().unwrap();
        for d in [&real, &real.join("runs"), &real.join("sources")] {
            assert_eq!(mode(d), 0o700, "{}", d.display());
        }
        assert_eq!(mode(&elsewhere), 0o775, "a link out of the home is not followed");

        // A fresh home, with its parents.
        let fresh = PulseHome::new(dir.path().join("a/b/home"));
        fresh.create_dirs().unwrap();
        for d in [fresh.root().to_path_buf(), fresh.logs_dir(), fresh.runs_dir()] {
            assert_eq!(mode(&d), 0o700, "{}", d.display());
        }

        // Not a directory: refused rather than used.
        let file = dir.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(PulseHome::new(&file).create_dirs().is_err());
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
