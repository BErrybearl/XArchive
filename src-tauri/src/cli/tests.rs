use super::*;

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn arguments_preserve_gui_only_when_empty_and_require_exact_options() {
    assert_eq!(parse(vec![]).unwrap(), None);
    assert_eq!(parse(args(&["--help"])).unwrap(), Some(Command::Help));
    assert_eq!(parse(args(&["-h"])).unwrap(), Some(Command::Help));
    assert_eq!(
        parse(args(&[
            "import-legacy",
            "--source",
            "合成 旧库.db",
            "--data",
            "隔离 目录"
        ]))
        .unwrap(),
        Some(Command::Import {
            source: "合成 旧库.db".into(),
            data: Some("隔离 目录".into())
        })
    );
    assert_eq!(
        parse(args(&["verify"])).unwrap(),
        Some(Command::Verify { data: None })
    );
    for invalid in [
        vec!["unknown"],
        vec!["--help", "verify"],
        vec!["import-legacy"],
        vec!["import-legacy", "--source"],
        vec!["import-legacy", "--source", "--data", "data"],
        vec!["verify", "--source", "old.db"],
        vec!["verify", "--data", ""],
        vec!["verify", "--data", "a", "--data", "b"],
        vec!["verify", "extra"],
    ] {
        assert!(parse(args(&invalid)).is_err(), "{invalid:?}");
    }
}

#[test]
fn default_data_is_next_to_exe_without_initialization_or_location_override() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let base = project.join(".runtime/cli-unit-tests");
    fs::create_dir_all(&base).unwrap();
    let dir = tempfile::tempdir_in(base).unwrap();
    let exe = dir.path().join("XArchive.exe");
    fs::write(dir.path().join("location.json"), r#"{"path":"other"}"#).unwrap();
    assert_eq!(data_root(None, &exe).unwrap(), dir.path().join("data"));
    assert!(!dir.path().join("data").exists());
}

#[test]
fn directory_lock_is_exclusive_and_stale_file_is_reusable() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/cli-unit-tests");
    fs::create_dir_all(&base).unwrap();
    let dir = tempfile::tempdir_in(base).unwrap();
    let lock = storage::DirectoryLock::acquire(dir.path()).unwrap();
    assert!(lock.holds(&dir.path().join(".")).unwrap());
    assert!(storage::DirectoryLock::acquire(dir.path()).is_err());
    drop(lock);
    assert!(storage::DirectoryLock::acquire(dir.path()).is_ok());
}

#[test]
fn gui_directory_switch_retains_old_lock_on_occupancy_or_persistence_failure() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/cli-unit-tests");
    fs::create_dir_all(&base).unwrap();
    let dir = tempfile::tempdir_in(base).unwrap();
    let old = dir.path().join("old");
    let next = dir.path().join("next");
    let mut gui = storage::DirectoryLock::acquire(&old).unwrap();
    let occupied = storage::DirectoryLock::acquire(&next).unwrap();
    assert!(
        gui.switch(&next, || -> storage::Result<()> {
            panic!("must reject before persistence")
        })
        .is_err()
    );
    assert!(storage::DirectoryLock::acquire(&old).is_err());
    drop(occupied);
    assert!(
        gui.switch(&next, || -> storage::Result<()> {
            Err(storage::StorageError::Invalid(
                "synthetic persistence failure".into(),
            ))
        })
        .is_err()
    );
    assert!(gui.holds(&old).unwrap());
    assert!(storage::DirectoryLock::acquire(&old).is_err());
    assert!(storage::DirectoryLock::acquire(&next).is_ok());
    gui.switch(&next, || Ok(())).unwrap();
    assert!(storage::DirectoryLock::acquire(&old).is_ok());
    assert!(storage::DirectoryLock::acquire(&next).is_err());
}

#[test]
fn gui_startup_errors_return_before_launch_and_release_any_acquired_lock() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/cli-unit-tests");
    fs::create_dir_all(&base).unwrap();
    let dir = tempfile::tempdir_in(base).unwrap();
    let lock = storage::DirectoryLock::acquire(dir.path()).unwrap();
    let result = crate::with_startup_directory(Ok(dir.path().to_owned()), |_| {
        panic!("occupied directory must not launch GUI")
    });
    assert!(result.unwrap_err().contains("已被其他 XArchive"));
    drop(lock);
    let blocked = dir.path().join("file");
    fs::write(&blocked, b"not a directory").unwrap();
    assert!(
        crate::with_startup_directory(Ok(blocked), |_| panic!(
            "invalid directory must not launch GUI"
        ))
        .is_err()
    );
    assert!(
        crate::with_startup_directory(Ok(dir.path().to_owned()), |_| Err(
            "synthetic startup failure".into()
        ))
        .is_err()
    );
    assert!(storage::DirectoryLock::acquire(dir.path()).is_ok());
}

#[test]
#[ignore = "subprocess fixture for directory lock tests"]
fn directory_lock_child_process() {
    let Some(root) = std::env::var_os("XARCHIVE_TEST_LOCK_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    legacy::validate_destination(&root).unwrap();
    let _lock = storage::DirectoryLock::acquire(&root).unwrap();
    fs::write(root.join("ready"), b"locked").unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn directory_lock_is_released_after_forced_process_exit() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/cli-unit-tests");
    fs::create_dir_all(&base).unwrap();
    let dir = tempfile::tempdir_in(base).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cli::tests::directory_lock_child_process",
            "--ignored",
        ])
        .env("XARCHIVE_TEST_LOCK_ROOT", dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !dir.path().join("ready").exists() && Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let ready = dir.path().join("ready").exists();
    let exclusive = ready && storage::DirectoryLock::acquire(dir.path()).is_err();
    // Force termination, deliberately bypassing Rust destructors.
    let _ = child.kill();
    child.wait().unwrap();
    assert!(
        ready && exclusive,
        "child must hold the lock before forced termination"
    );
    assert!(dir.path().join(".xarchive.lock").is_file());
    assert!(storage::DirectoryLock::acquire(dir.path()).is_ok());
}
