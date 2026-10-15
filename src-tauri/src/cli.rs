//! Headless entry point. Nothing here constructs a Tauri runtime or WebView.
use crate::{legacy, storage};
use serde::Serialize;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{SystemTime, UNIX_EPOCH},
};

const HELP: &str = "XArchive 命令行（不打开窗口）\n\
  XArchive.exe import-legacy --source <旧库.db> [--data <数据目录>]\n\
  XArchive.exe verify [--data <数据目录>]\n\
  XArchive.exe --help\n\
数据目录缺省为 EXE 旁 data/；界面改选过位置时请显式指定 --data。\n\
导入自动初始化（默认结构 C），复用界面的备份、导入与核对实现。\n\
校验只报告缺失、变化、孤立及临时文件，不修复。\n\
报告保存到数据目录 reports/；同一目录的界面须先关闭。\n\
退出码：0 通过，2 核对/校验未通过，1 参数或执行错误。\n";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Import {
        source: PathBuf,
        data: Option<PathBuf>,
    },
    Verify {
        data: Option<PathBuf>,
    },
}

fn parse(arguments: Vec<OsString>) -> Result<Option<Command>, String> {
    if arguments.is_empty() {
        return Ok(None);
    }
    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--help" | "-h")) {
        return Ok(Some(Command::Help));
    }
    let importing = match arguments[0].to_str() {
        Some("import-legacy") => true,
        Some("verify") => false,
        _ => return Err("未知命令；使用 --help 查看用法".into()),
    };
    let mut source = None;
    let mut data = None;
    let mut args = arguments.into_iter().skip(1);
    while let Some(flag) = args.next() {
        let slot = match flag.to_str() {
            Some("--source") if importing => &mut source,
            Some("--data") => &mut data,
            _ => return Err("参数不受支持；使用 --help 查看用法".into()),
        };
        if slot.is_some() {
            return Err("参数重复；每个选项只能指定一次".into());
        }
        let value = args.next().ok_or("参数缺少路径值")?;
        if value.is_empty() || value.to_string_lossy().starts_with("--") {
            return Err("参数缺少路径值".into());
        }
        *slot = Some(PathBuf::from(value));
    }
    Ok(Some(if importing {
        Command::Import {
            source: source.ok_or("import-legacy 必须指定 --source <旧库.db>")?,
            data,
        }
    } else {
        Command::Verify { data }
    }))
}

/// None preserves normal GUI startup; every argument, including invalid ones, stays headless.
pub fn run_if_requested() -> Option<u8> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.is_empty() {
        return None;
    }
    attach_parent_console();
    let result = parse(arguments).and_then(|command| {
        let command = command.expect("arguments are not empty");
        if command == Command::Help {
            output(HELP);
            return Ok(0);
        }
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        execute(command, &executable, &mut |line| output(line))
    });
    Some(match result {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "错误：{error}");
            1
        }
    })
}

fn output(text: &str) {
    // Missing consoles and closed pipes must not prevent report persistence or determine success.
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{text}");
    let _ = stdout.flush();
}

#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE,
            STD_OUTPUT_HANDLE, SetStdHandle,
        },
    };
    // AttachConsole may replace inherited redirection handles. Save and restore them.
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        let stderr = GetStdHandle(STD_ERROR_HANDLE);
        if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            for (kind, handle) in [(STD_OUTPUT_HANDLE, stdout), (STD_ERROR_HANDLE, stderr)] {
                if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                    SetStdHandle(kind, handle);
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn data_root(data: Option<PathBuf>, executable: &Path) -> Result<PathBuf, String> {
    let path = match data {
        Some(path) => std::path::absolute(path).map_err(|e| e.to_string())?,
        None => storage::portable_root(executable).map_err(|e| e.to_string())?,
    };
    // This also enforces the existing .runtime boundary in development builds, before any writes.
    legacy::validate_destination(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

fn execute(
    command: Command,
    executable: &Path,
    notify: &mut impl FnMut(&str),
) -> Result<u8, String> {
    let data = match &command {
        Command::Import { data, .. } | Command::Verify { data } => data.clone(),
        Command::Help => {
            notify(HELP);
            return Ok(0);
        }
    };
    let root = data_root(data, executable)?;
    if matches!(command, Command::Verify { .. }) && !root.is_dir() {
        return Err("数据目录不存在，请先初始化或导入".into());
    }
    let _lock = storage::DirectoryLock::acquire(&root).map_err(|e| e.to_string())?;
    match command {
        Command::Import { source, .. } => {
            let result = legacy::import(&root, &source, &AtomicBool::new(false), |p| {
                let percent = if p.total > 0 {
                    (p.done as f64 / p.total as f64 * 100.0).min(100.0)
                } else if !p.running {
                    100.0
                } else {
                    0.0
                };
                notify(&format!(
                    "{} {:.1}% ({}/{})",
                    p.stage, percent, p.done, p.total
                ));
            });
            let mut report = result.unwrap_or_else(|error| legacy::Report {
                destination: storage::friendly_path(&root).to_string_lossy().into_owned(),
                error: Some(error.to_string()),
                ..Default::default()
            });
            let (json, md) = report_paths(&root, "legacy-import")?;
            report.report_path = storage::friendly_path(&json).to_string_lossy().into_owned();
            let markdown = import_markdown(&report);
            save_reports(&json, &md, &report, &markdown)?;
            notify(&markdown);
            notify(&format!(
                "JSON：{}\nMarkdown：{}",
                json.display(),
                md.display()
            ));
            Ok(if report.error.is_some() {
                1
            } else if report.passed {
                0
            } else {
                2
            })
        }
        Command::Verify { .. } => {
            notify("完整性检查 0.0%");
            let report = storage::Store::open_readonly(&root)
                .and_then(|store| store.inspect())
                .map_err(|e| e.to_string())?;
            let passed = report.missing_files.is_empty()
                && report.changed_files.is_empty()
                && report.untracked_files.is_empty()
                && report.temporary_files == 0;
            let mut markdown = format!(
                "# 完整性检查\n\n结果：{}\n\n临时文件/目录：{}\n",
                verdict(passed),
                report.temporary_files
            );
            for (title, items) in [
                ("缺失文件", &report.missing_files),
                ("变化文件", &report.changed_files),
                ("孤立文件", &report.untracked_files),
            ] {
                list_summary(&mut markdown, title, items);
            }
            let (json, md) = report_paths(&root, "verify")?;
            save_reports(&json, &md, &report, &markdown)?;
            notify("完整性检查 100.0%");
            notify(&markdown);
            notify(&format!(
                "JSON：{}\nMarkdown：{}",
                json.display(),
                md.display()
            ));
            Ok(if passed { 0 } else { 2 })
        }
        Command::Help => unreachable!(),
    }
}

fn report_paths(root: &Path, prefix: &str) -> Result<(PathBuf, PathBuf), String> {
    let directory = root.join("reports");
    storage::check_path(&directory).map_err(|e| e.to_string())?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    let stem = format!(
        "{prefix}-{}-{:09}-{}",
        time.as_secs(),
        time.subsec_nanos(),
        std::process::id()
    );
    Ok((
        directory.join(format!("{stem}.json")),
        directory.join(format!("{stem}.md")),
    ))
}

fn save_reports(
    json: &Path,
    md: &Path,
    report: &impl Serialize,
    markdown: &str,
) -> Result<(), String> {
    fn save(path: &Path, bytes: &[u8]) -> Result<(), String> {
        storage::check_path(path).map_err(|e| e.to_string())?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(|e| e.to_string())?;
        temporary.write_all(bytes).map_err(|e| e.to_string())?;
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        temporary
            .persist_noclobber(path)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    let bytes = serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?;
    save(json, &bytes)?;
    save(md, markdown.as_bytes())
}

fn verdict(passed: bool) -> &'static str {
    if passed { "通过" } else { "未通过" }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn list_summary(markdown: &mut String, title: &str, items: &[String]) {
    use std::fmt::Write;
    let _ = write!(markdown, "\n## {title}：{} 项\n\n", items.len());
    for item in items.iter().take(20) {
        let _ = writeln!(markdown, "- {}", cell(item));
    }
    if items.len() > 20 {
        markdown.push_str("\n仅列前 20 项，完整清单见 JSON。\n");
    }
}

fn import_markdown(report: &legacy::Report) -> String {
    use std::fmt::Write;
    let c = &report.counts;
    let mut text = format!(
        "# 旧库导入核对报告\n\n结果：{}\n\n| 项目 | 旧库值 | 新库值 | 核对 |\n| --- | ---: | ---: | --- |\n",
        verdict(report.passed)
    );
    for (label, old, new) in [
        ("推文", c.old_tweets, c.imported_tweets),
        (
            "唯一媒体（按账号和实际字节核算）",
            c.expected_media,
            c.imported_media,
        ),
        ("来源", c.old_sources, c.imported_sources),
    ] {
        let _ = writeln!(
            text,
            "| {label} | {old} | {new} | {} |",
            verdict(old == new && report.error.is_none())
        );
    }
    let _ = write!(
        text,
        "\n旧库物理媒体行：{}；已核对来源摘要：{}。\n\n旧库文件 SHA-256（备份前/后）：{} / {}\n",
        c.old_media_rows,
        c.digests_checked,
        report.source_sha256_before,
        report.source_sha256_after
    );
    if let Some(error) = &report.error {
        let _ = writeln!(text, "\n执行错误：{}", cell(error));
    }
    let _ = write!(
        text,
        "\n## 摘要逐项结果\n\n| 来源 | 旧库声明 SHA-256 | BLOB SHA-256 / 字节 | 新库文件 SHA-256 / 字节 | 新库索引 SHA-256 / 字节 | 结果 |\n| --- | --- | --- | --- | --- | --- |\n"
    );
    for d in &report.digests {
        let _ = writeln!(
            text,
            "| {} | {} | {} / {} | {} / {} | {} / {} | {} |",
            cell(&d.source),
            cell(d.legacy_declared.as_deref().unwrap_or("未声明")),
            d.legacy_sha256,
            d.legacy_bytes,
            d.file_sha256.as_deref().unwrap_or("未读到"),
            d.file_bytes.map(|n| n.to_string()).unwrap_or_default(),
            d.index_sha256.as_deref().unwrap_or("未读到"),
            d.index_bytes.map(|n| n.to_string()).unwrap_or_default(),
            verdict(d.passed)
        );
    }
    list_summary(&mut text, "缺失清单", &report.missing);
    list_summary(&mut text, "不一致清单", &report.mismatches);
    let _ = writeln!(
        text,
        "\n待重试队列：{} 组；停止：{}。",
        report.pending.len(),
        report.stopped
    );
    for notice in &report.notices {
        let _ = writeln!(text, "\n{}", cell(notice));
    }
    text
}

#[cfg(test)]
mod tests;
