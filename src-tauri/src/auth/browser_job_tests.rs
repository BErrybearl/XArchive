use super::*;
use crate::auth::{AuthError, browser};
use std::{
    fs, thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::WaitForSingleObject,
};

const HELPER: &str = "auth::browser::job::tests::job_fixture_process";

fn fixture_base() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/browser-job-tests")
}
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Job fixture"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

// Run only through this test binary's explicit --exact/--ignored invocation.
// A normal cargo test never executes a real browser, shell or network utility.
#[test]
#[ignore = "subprocess fixture for Job Object tests"]
fn job_fixture_process() {
    if let Some(root) = std::env::var_os("XARCHIVE_JOB_TEST_CHILD") {
        let root = std::path::PathBuf::from(root);
        assert!(root.starts_with(fixture_base()));
        fs::write(root.join("child-ready"), b"ready").unwrap();
        until(|| root.join("release-child").exists());
        fs::write(root.join("child-done"), b"done").unwrap();
        return;
    }
    let root = fixture_base().join(std::process::id().to_string());
    fs::create_dir_all(fixture_base()).unwrap();
    fs::create_dir(&root).unwrap();
    let mut child = browser::command(&std::env::current_exe().unwrap())
        .args(["--exact", HELPER, "--ignored"])
        .env("XARCHIVE_JOB_TEST_CHILD", &root)
        .spawn()
        .unwrap();
    until(|| root.join("child-ready").exists());
    fs::write(root.join("parent-ready"), b"ready").unwrap();
    until(|| root.join("release-parent").exists());
    // Deliberately let the child outlive this original process.
    let _ = child.try_wait().unwrap();
}

struct Fixture {
    process: Option<Process>,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let process = Process::start(
            &std::env::current_exe().unwrap(),
            &["--exact".into(), HELPER.into(), "--ignored".into()],
        )
        .unwrap();
        Self::from_process(process)
    }
    fn from_process(process: Process) -> Self {
        assert!(process._pid != 0);
        let root = fixture_base().join(process._pid.to_string());
        let fixture = Self {
            process: Some(process),
            root,
        };
        until(|| fixture.root.join("parent-ready").exists());
        fixture
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::write(self.root.join("release-parent"), b"release");
        let _ = fs::write(self.root.join("release-child"), b"release");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let done = if let Some(process) = &self.process {
                process.running().is_ok_and(|running| !running)
            } else {
                self.root.join("child-done").exists()
            };
            if done {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // root is derived solely from this fixture's own PID under .runtime.
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn job_tracks_descendants_after_parent_exit_and_locks_cover_handoff() {
    let fixture = Fixture::new();
    let process = fixture.process.as_ref().unwrap();
    use windows_sys::Win32::System::JobObjects::{
        JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation,
    };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    assert_ne!(
        unsafe {
            QueryInformationJobObject(
                process.job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
                null_mut(),
            )
        },
        0
    );
    assert_eq!(
        limits.BasicLimitInformation.LimitFlags
            & (JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_BREAKAWAY_OK
                | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK),
        0
    );
    // A valid handle of the wrong object type makes the Job query fail. It must
    // remain an error even though our original process is still alive.
    let unknown_job = Process {
        _process: process._process.try_clone().unwrap(),
        _pid: process._pid,
        job: process._process.try_clone().unwrap(),
    };
    assert!(unknown_job.running().is_err());
    assert!(process.running().unwrap());
    assert!(
        browser::occupied(
            process.running().map_err(|_| AuthError::BrowserConnection),
            || panic!("an active Job needs no lock probe")
        )
        .unwrap()
    );
    fs::write(fixture.root.join("release-parent"), b"release").unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(process._process.as_raw_handle(), 5000) },
        WAIT_OBJECT_0
    );
    assert!(
        process.running().unwrap(),
        "the descendant must remain tracked"
    );

    let lockfile = fixture.root.join("lockfile");
    fs::write(&lockfile, b"synthetic lock").unwrap();
    use std::os::windows::fs::OpenOptionsExt;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&lockfile)
        .unwrap();
    fs::write(fixture.root.join("release-child"), b"release").unwrap();
    until(|| !process.running().unwrap());
    let in_use = || {
        browser::occupied(
            process.running().map_err(|_| AuthError::BrowserConnection),
            || browser::profile_in_use(&fixture.root),
        )
    };
    assert!(
        in_use().unwrap(),
        "an existing instance's lock must cover handoff"
    );
    drop(lock);
    assert!(
        !in_use().unwrap(),
        "stale lock files must not imply occupancy"
    );
}

#[test]
fn dropping_job_tracker_keeps_interactive_processes_alive() {
    let mut fixture = Fixture::new();
    let handle = fixture
        .process
        .as_ref()
        .unwrap()
        ._process
        .try_clone()
        .unwrap();
    drop(fixture.process.take()); // The last Job handle is closed here.
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle(), 200) },
        WAIT_TIMEOUT
    );
    fs::write(fixture.root.join("release-parent"), b"release").unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) },
        WAIT_OBJECT_0
    );
    assert!(!fixture.root.join("child-done").exists());
    fs::write(fixture.root.join("release-child"), b"release").unwrap();
    until(|| fixture.root.join("child-done").exists());
}

#[test]
fn reader_termination_waits_for_the_entire_owned_process_tree() {
    let reader = Reader::start(
        &std::env::current_exe().unwrap(),
        &["--exact".into(), HELPER.into(), "--ignored".into()],
    )
    .unwrap();
    let mut fixture = Fixture::from_process(reader.0);
    let reader = Reader(fixture.process.take().unwrap());
    assert!(reader.running().unwrap());
    reader.terminate().unwrap();
    until(|| !reader.running().unwrap());
    assert_eq!(
        unsafe { WaitForSingleObject(reader.0._process.as_raw_handle(), 0) },
        WAIT_OBJECT_0
    );
    assert!(
        !fixture.root.join("child-done").exists(),
        "the child was terminated with its reader Job"
    );
    fixture.process = Some(reader.0);
}

#[test]
fn job_start_rejects_invalid_or_missing_executable() {
    assert!(Process::start(&fixture_base().join("missing-browser.exe"), &[]).is_err());
    assert!(Process::start(Path::new("bad\0path"), &[]).is_err());
    assert!(command_line(OsStr::new("browser.exe"), &["bad\0arg".into()]).is_err());
}

#[test]
fn native_browser_arguments_quote_spaces_unicode_quotes_and_trailing_slashes() {
    let line = command_line(
        OsStr::new(r"C:\Program Files\browser.exe"),
        &[
            OsString::from(""),
            OsString::from("--user-data-dir=C:\\合成 资料\\"),
            OsString::from("literal\\\"quote"),
        ],
    )
    .unwrap();
    assert_eq!(
        String::from_utf16(&line[..line.len() - 1]).unwrap(),
        "\"C:\\Program Files\\browser.exe\" \"\" \"--user-data-dir=C:\\合成 资料\\\\\" \"literal\\\\\\\"quote\""
    );
}
