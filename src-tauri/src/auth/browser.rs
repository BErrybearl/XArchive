use super::{AuthError, Credentials, Result, capture_cookies, cdp, check};
#[cfg(not(windows))]
use std::process::Child;
#[cfg(any(not(windows), test))]
use std::process::{Command, Stdio};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
#[path = "browser_job.rs"]
mod job;

pub(super) trait LoginBrowser: Send {
    fn in_use(&mut self, profile: &Path) -> Result<bool>;
    fn read(&mut self, profile: &Path) -> Result<Credentials>;
}

pub(super) fn login_args(profile: &Path, proxy: &str) -> Vec<OsString> {
    let mut args = vec![
        profile_arg(profile),
        // Skip first-run onboarding for the dedicated profile.
        "--no-first-run".into(),
        // Skip checking whether this browser is the Windows default.
        "--no-default-browser-check".into(),
        // Disable browser account synchronization; X cookies remain in this profile.
        "--disable-sync".into(),
        // Suppress browser background services' network traffic during login.
        "--disable-background-networking".into(),
        // Suppress component updater downloads competing with the login page.
        "--disable-component-update".into(),
        // Do not install the browser's default bundled apps into a new profile.
        "--disable-default-apps".into(),
    ];
    if !proxy.is_empty() {
        args.push(format!("--proxy-server={proxy}").into());
    }
    args.push("https://x.com/login".into());
    args
}
pub(super) fn headless_args(profile: &Path) -> Vec<OsString> {
    vec![
        "--headless=new".into(),
        "--remote-debugging-port=0".into(),
        "--remote-debugging-address=127.0.0.1".into(),
        profile_arg(profile),
        "about:blank".into(),
    ]
}
fn profile_arg(profile: &Path) -> OsString {
    let mut arg = OsString::from("--user-data-dir=");
    arg.push(profile.as_os_str());
    arg
}
#[cfg(any(not(windows), test))]
fn command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // Suppress only console windows.
    }
    command
}
fn find_browser() -> Result<PathBuf> {
    // Inspect executable locations only; never inspect a user's browser data.
    for suffix in [
        "Microsoft/Edge/Application/msedge.exe",
        "Google/Chrome/Application/chrome.exe",
    ] {
        for base in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(base) {
                let candidate = PathBuf::from(base).join(suffix);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    Err(AuthError::BrowserStart)
}

pub(super) struct Browser {
    executable: PathBuf,
    #[cfg(windows)]
    interactive: job::Process,
    #[cfg(not(windows))]
    interactive: Child,
}
impl Browser {
    pub(super) fn open(profile: &Path, proxy: &str) -> Result<Self> {
        let executable = find_browser()?;
        #[cfg(windows)]
        let interactive = job::Process::start(&executable, &login_args(profile, proxy))
            .map_err(|_| AuthError::BrowserStart)?;
        #[cfg(not(windows))]
        let interactive = command(&executable)
            .args(login_args(profile, proxy))
            .spawn()
            .map_err(|_| AuthError::BrowserStart)?;
        Ok(Self {
            executable,
            interactive,
        })
    }
}
impl LoginBrowser for Browser {
    fn in_use(&mut self, profile: &Path) -> Result<bool> {
        // The Job includes descendants even after the launcher exits. If Chromium
        // hands the window to an existing instance, exclusive profile locks cover it.
        #[cfg(windows)]
        let running = self.interactive.running();
        #[cfg(not(windows))]
        let running = self.interactive.try_wait().map(|status| status.is_none());
        occupied(running.map_err(|_| AuthError::BrowserConnection), || {
            profile_in_use(profile)
        })
    }
    fn read(&mut self, profile: &Path) -> Result<Credentials> {
        if self.in_use(profile)? {
            return Err(AuthError::BrowserInUse);
        }
        let mut reader = Headless::start(&self.executable, profile)?;
        let result = read_and_close(&mut reader);
        // Do not commit if the reader or a concurrently reopened browser owns the profile.
        if self.in_use(profile)? {
            return Err(AuthError::BrowserInUse);
        }
        result
    }
}
pub(super) fn occupied(
    process_running: Result<bool>,
    other_signals: impl FnOnce() -> Result<bool>,
) -> Result<bool> {
    if process_running? {
        return Ok(true);
    }
    other_signals()
}
pub(super) fn profile_in_use(profile: &Path) -> Result<bool> {
    check(profile)?;
    locks_in_use(profile)
}

// Persistent LOCK/lockfile files can remain after exit: test exclusivity, not mere existence.
pub(super) fn locks_in_use(profile: &Path) -> Result<bool> {
    let mut directories = vec![profile.to_owned()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).map_err(|_| AuthError::BrowserConnection)? {
            let entry = entry.map_err(|_| AuthError::BrowserConnection)?;
            let path = entry.path();
            check(&path)?;
            let kind = entry
                .file_type()
                .map_err(|_| AuthError::BrowserConnection)?;
            if kind.is_dir() {
                directories.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "lock"
                    | "lockfile"
                    | "singletonlock"
                    | "singletonsocket"
                    | "singletoncookie"
                    | "cookies"
            ) {
                continue;
            }
            let mut options = OpenOptions::new();
            options.read(true).write(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(0);
            }
            match options.open(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Ok(true),
            }
        }
    }
    Ok(false)
}

fn wait_exit(reader: &mut Headless, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        if !reader.running()? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(50));
    }
}
pub(super) trait CookieReader {
    fn cookies(&mut self) -> Result<serde_json::Value>;
    fn close(&mut self) -> Result<()>;
}
pub(super) fn read_and_close(reader: &mut impl CookieReader) -> Result<Credentials> {
    let result = reader
        .cookies()
        .and_then(|mut value| capture_cookies(&mut value));
    // Always close on transport, parsing, missing-cookie and successful paths.
    reader.close()?;
    result
}
struct Headless {
    #[cfg(windows)]
    child: job::Reader,
    #[cfg(not(windows))]
    child: Child,
    cdp: Option<cdp::Cdp>,
    stopped: bool,
}
impl Headless {
    fn start(executable: &Path, profile: &Path) -> Result<Self> {
        let port_file = profile.join("DevToolsActivePort");
        check(&port_file)?;
        match fs::remove_file(&port_file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(AuthError::BrowserConnection),
        }
        #[cfg(windows)]
        let child = job::Reader::start(executable, &headless_args(profile))
            .map_err(|_| AuthError::BrowserConnection)?;
        #[cfg(not(windows))]
        let child = command(executable)
            .args(headless_args(profile))
            .spawn()
            .map_err(|_| AuthError::BrowserConnection)?;
        let mut reader = Self {
            child,
            cdp: None,
            stopped: false,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !reader.running()? {
                return Err(AuthError::BrowserConnection);
            }
            if port_file.exists() {
                let bytes = super::bounded_read(&port_file)?;
                if let Ok(raw) = std::str::from_utf8(&bytes) {
                    if let Ok((port, path)) = cdp::active_port(raw) {
                        reader.cdp = Some(cdp::Cdp::connect(port, &path)?);
                        return Ok(reader);
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(AuthError::BrowserConnection);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    fn running(&mut self) -> Result<bool> {
        #[cfg(windows)]
        let running = self.child.running();
        #[cfg(not(windows))]
        let running = self.child.try_wait().map(|status| status.is_none());
        running.map_err(|_| AuthError::BrowserConnection)
    }
}
impl CookieReader for Headless {
    fn cookies(&mut self) -> Result<serde_json::Value> {
        self.cdp.as_mut().ok_or(AuthError::BrowserConnection)?.call(
            "Storage.getCookies",
            serde_json::json!({}),
            Duration::from_secs(5),
        )
    }
    fn close(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        stop_reader(self)?;
        self.stopped = true;
        Ok(())
    }
}
impl Drop for Headless {
    fn drop(&mut self) {
        if self.close().is_err() {
            let _ = self.terminate();
        }
    }
}

// Only the reader process supports termination; the interactive browser has no kill path.
pub(super) trait ReaderProcess {
    fn request_close(&mut self);
    fn wait(&mut self, timeout: Duration) -> Result<bool>;
    fn terminate(&mut self) -> Result<()>;
}
pub(super) fn stop_reader(process: &mut impl ReaderProcess) -> Result<()> {
    process.request_close();
    if !process.wait(Duration::from_secs(3))? {
        process.terminate()?;
        if !process.wait(Duration::from_secs(2))? {
            return Err(AuthError::BrowserConnection);
        }
    }
    Ok(())
}
impl ReaderProcess for Headless {
    fn request_close(&mut self) {
        if let Some(cdp) = &mut self.cdp {
            cdp.close_browser();
        }
    }
    fn wait(&mut self, timeout: Duration) -> Result<bool> {
        wait_exit(self, timeout)
    }
    fn terminate(&mut self) -> Result<()> {
        #[cfg(windows)]
        let result = self.child.terminate();
        #[cfg(not(windows))]
        let result = self.child.kill();
        result.map_err(|_| AuthError::BrowserConnection)
    }
}
