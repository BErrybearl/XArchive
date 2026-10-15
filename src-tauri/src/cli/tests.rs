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
