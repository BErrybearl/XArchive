//! Real process tests, exclusively on generated SQLite/media under .runtime.
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn isolated() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/cli-process-tests");
    fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

fn fixture(path: &Path, bad_digest: bool) {
    let db = Connection::open(path).unwrap();
    db.execute_batch("CREATE TABLE users(user_id TEXT,screen_name TEXT,display_name TEXT);
        CREATE TABLE tweets(tweet_id TEXT,user_id TEXT,text TEXT,created_at TEXT);
        CREATE TABLE media(id INTEGER PRIMARY KEY,user_id TEXT,tweet_id TEXT,filename TEXT,url TEXT,media_type TEXT,data BLOB,sha256 TEXT);
        CREATE TABLE media_aliases(user_id TEXT,tweet_id TEXT,filename TEXT,url TEXT,media_type TEXT,canonical_id INTEGER,sha256 TEXT);
        INSERT INTO users VALUES('u','synthetic','合成用户');
        INSERT INTO tweets VALUES('1','u','合成推文','2026-01-01'),('2','u','重复媒体来源','2026-01-02');").unwrap();
    let bytes = b"\x89PNG\r\n\x1a\n synthetic payload";
    let hash = if bad_digest {
        "0".repeat(64)
    } else {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    db.execute("INSERT INTO media VALUES(1,'u','1','1_0.png','https://example.invalid/1.png','photo',?1,?2)", params![bytes.as_slice(), hash]).unwrap();
    db.execute("INSERT INTO media_aliases VALUES('u','2','2_0.png','https://example.invalid/2.png','photo',1,?1)", [&hash]).unwrap();
}

fn run(mut command: Command) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("CLI did not exit (possible GUI startup)");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xarchive"))
}
fn import(source: &Path, data: &Path) -> Output {
    let mut command = command();
    command
        .arg("import-legacy")
        .arg("--source")
        .arg(source)
        .arg("--data")
        .arg(data);
    run(command)
}
fn verify(data: &Path) -> Output {
    let mut command = command();
    command.arg("verify").arg("--data").arg(data);
    run(command)
}
fn copies(data: &Path) -> usize {
    fs::read_dir(data.join("temporary"))
        .map(|d| {
            d.filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("legacy-import-")
            })
            .count()
        })
        .unwrap_or(0)
}
fn json_reports(data: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(data.join("reports"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json")
                && p.file_name().unwrap().to_string_lossy().starts_with(prefix)
        })
        .collect();
    paths.sort();
    paths
}

#[test]
fn process_import_reports_counts_digests_and_is_idempotent() {
    let dir = isolated();
    let source = dir.path().join("合成 旧库.db");
    fixture(&source, false);
    let before = fs::read(&source).unwrap();
    let data = dir.path().join("新 数据");
    let output = import(&source, &data);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("核对来源与媒体摘要")
            && stdout.contains('%')
            && stdout.contains("摘要逐项结果")
    );
    let paths = json_reports(&data, "legacy-import-");
    assert_eq!(paths.len(), 1);
    let report: serde_json::Value = serde_json::from_slice(&fs::read(&paths[0]).unwrap()).unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["counts"]["old_tweets"], 2);
    assert_eq!(report["counts"]["imported_tweets"], 2);
    assert_eq!(report["counts"]["expected_media"], 1);
    assert_eq!(report["counts"]["imported_media"], 1);
    assert_eq!(report["counts"]["old_sources"], 2);
    assert_eq!(report["digests"].as_array().unwrap().len(), 2);
    assert!(
        report["digests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["passed"] == true)
    );
    assert!(paths[0].with_extension("md").is_file());
    assert_eq!(copies(&data), 0);
    assert_eq!(verify(&data).status.code(), Some(0));
    assert_eq!(
        xarchive_lib::storage::status(&data).unwrap().layout,
        xarchive_lib::storage::Layout::C
    );
    assert_eq!(import(&source, &data).status.code(), Some(0));
    assert_eq!(json_reports(&data, "legacy-import-").len(), 2);
    let status = xarchive_lib::storage::status(&data).unwrap();
    assert_eq!((status.tweets, status.media, status.sources), (2, 1, 2));
    assert_eq!(fs::read(&source).unwrap(), before);
}

#[test]
fn process_unpassed_is_two_and_execution_errors_are_one_with_reports() {
    let dir = isolated();
    let source = dir.path().join("bad-digest.db");
    fixture(&source, true);
    let data = dir.path().join("bad-data");
    assert_eq!(import(&source, &data).status.code(), Some(2));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&json_reports(&data, "legacy-import-")[0]).unwrap())
            .unwrap();
    assert_eq!(copies(&data), 1);
    assert_eq!(report["passed"], false);
    assert!(!report["mismatches"].as_array().unwrap().is_empty());
    assert!(
        report["digests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["passed"] == false)
    );
    let missing = dir.path().join("missing.db");
    let error_data = dir.path().join("error-data");
    assert_eq!(import(&missing, &error_data).status.code(), Some(1));
    assert!(
        json_reports(&error_data, "legacy-import-")[0]
            .with_extension("md")
            .is_file()
    );
    let invalid = dir.path().join("invalid.db");
    fs::write(&invalid, b"not sqlite").unwrap();
    assert_eq!(import(&invalid, &error_data).status.code(), Some(1));
}

#[test]
fn process_verify_reports_all_four_categories_without_repair() {
    let dir = isolated();
    let data = dir.path().join("data");
    drop(xarchive_lib::storage::Store::open(&data).unwrap());
    assert_eq!(verify(&data).status.code(), Some(0));
    let source = dir.path().join("legacy.db");
    fixture(&source, false);
    assert_eq!(import(&source, &data).status.code(), Some(0));
    assert_eq!(verify(&data).status.code(), Some(0));
    fs::write(data.join("temporary/leftover.tmp"), b"x").unwrap();
    assert_eq!(verify(&data).status.code(), Some(2));
    let db = Connection::open(data.join("archive.sqlite3")).unwrap();
    let relative: String = db
        .query_row("SELECT relative_path FROM media_entities", [], |r| r.get(0))
        .unwrap();
    drop(db);
    let media = data.join(relative);
    let original = fs::read(&media).unwrap();
    let mut changed = original.clone();
    changed[0] ^= 1;
    fs::write(&media, &changed).unwrap();
    let orphan = data.join("media/orphan.png");
    fs::write(&orphan, b"orphan").unwrap();
    let index_before = fs::read(data.join("archive.sqlite3")).unwrap();
    assert_eq!(verify(&data).status.code(), Some(2));
    assert_eq!(
        fs::read(data.join("archive.sqlite3")).unwrap(),
        index_before
    );
    assert_eq!(fs::read(&media).unwrap(), changed);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(json_reports(&data, "verify-").last().unwrap()).unwrap())
            .unwrap();
    assert_eq!(report["changed_files"].as_array().unwrap().len(), 1);
    assert_eq!(report["untracked_files"].as_array().unwrap().len(), 1);
    assert!(report["temporary_files"].as_u64().unwrap() > 0);
    fs::remove_file(&media).unwrap();
    let output = verify(&data);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("缺失文件：1")
    );
    assert!(!media.exists() && orphan.exists());
}

#[test]
fn process_help_invalid_arguments_and_exe_relative_default() {
    let mut help = command();
    help.arg("--help");
    let output = run(help);
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8(output.stdout).unwrap().contains("命令行"));
    for args in [
        vec!["unknown"],
        vec!["import-legacy"],
        vec!["verify", "--data"],
        vec!["verify", "--source", "no.db"],
    ] {
        let mut cmd = command();
        cmd.args(args);
        assert_eq!(run(cmd).status.code(), Some(1));
    }
    let dir = isolated();
    let exe = dir.path().join("XArchive.exe");
    fs::copy(env!("CARGO_BIN_EXE_xarchive"), &exe).unwrap();
    let source = dir.path().join("old.db");
    fixture(&source, false);
    fs::write(
        dir.path().join("location.json"),
        r#"{"path":"do-not-open"}"#,
    )
    .unwrap();
    let mut cmd = Command::new(&exe);
    cmd.arg("import-legacy").arg("--source").arg(source);
    assert_eq!(run(cmd).status.code(), Some(0));
    assert!(dir.path().join("data/archive.sqlite3").is_file());
    assert!(!dir.path().join("do-not-open").exists());
}

#[cfg(windows)]
#[test]
fn process_rejects_gui_directory_lock_and_no_console_still_saves_reports() {
    use std::os::windows::{fs::OpenOptionsExt, process::CommandExt};
    let dir = isolated();
    let data = dir.path().join("data");
    drop(xarchive_lib::storage::Store::open(&data).unwrap());
    let source = dir.path().join("old.db");
    fixture(&source, false);
    let index = fs::read(data.join("archive.sqlite3")).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(data.join(".xarchive.lock"))
        .unwrap();
    for output in [import(&source, &data), verify(&data)] {
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("已被其他 XArchive")
        );
    }
    assert_eq!(fs::read(data.join("archive.sqlite3")).unwrap(), index);
    assert!(!data.join("reports").exists());
    drop(lock);
    let mut cmd = command();
    cmd.arg("import-legacy")
        .arg("--source")
        .arg(source)
        .arg("--data")
        .arg(&data)
        .creation_flags(0x08000000)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    assert_eq!(cmd.status().unwrap().code(), Some(0));
    assert!(
        json_reports(&data, "legacy-import-")[0]
            .with_extension("md")
            .is_file()
    );
}
