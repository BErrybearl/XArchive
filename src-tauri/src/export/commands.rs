use super::*;
use crate::{LoginAccess, StorageAccess};
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

#[derive(Default)]
pub(crate) struct ExportAccess {
    cancel: AtomicBool,
    progress: Mutex<Progress>,
}
pub(super) fn with_storage_lock<T>(
    access: &StorageAccess,
    task: impl FnOnce() -> std::result::Result<T, String>,
) -> std::result::Result<T, String> {
    let _guard = access
        .0
        .try_lock()
        .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
    task()
}
fn settings_path() -> Result<PathBuf> {
    Ok(storage::default_root()?
        .parent()
        .ok_or_else(|| invalid("无法定位导出设置"))?
        .join("export-directory.json"))
}
fn remembered(path: &Path) -> Result<Option<PathBuf>> {
    storage::check_path(path)?;
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(storage::friendly_path(
        &serde_json::from_slice::<PathBuf>(&fs::read(path)?)
            .map_err(|_| invalid("导出目录设置格式无效"))?,
    )))
}
fn remember(path: &Path, directory: &Path) -> Result<()> {
    storage::check_path(path)?;
    fs::create_dir_all(path.parent().ok_or_else(|| invalid("无法定位导出设置"))?)?;
    atomic_write(
        path,
        &serde_json::to_vec(&storage::friendly_path(directory))
            .map_err(|_| invalid("无法保存导出目录"))?,
    )
}
#[tauri::command]
pub(crate) fn get_export_directory() -> std::result::Result<Option<String>, String> {
    remembered(&settings_path().map_err(|e| e.to_string())?)
        .map(|path| path.map(|p| p.to_string_lossy().into_owned()))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn open_export_directory(
    directory: String,
    access: tauri::State<'_, ExportAccess>,
) -> std::result::Result<(), String> {
    let path = PathBuf::from(&directory);
    let remembered =
        remembered(&settings_path().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let is_result = access
        .progress
        .lock()
        .map_err(|_| "导出状态暂不可用")?
        .accounts
        .iter()
        .any(|a| {
            paths::resolved(Path::new(&a.directory))
                .ok()
                .zip(paths::resolved(&path).ok())
                .is_some_and(|(a, b)| a == b)
        });
    let is_remembered = remembered
        .as_ref()
        .and_then(|p| paths::resolved(p).ok())
        .zip(paths::resolved(&path).ok())
        .is_some_and(|(a, b)| a == b);
    if !is_result && !is_remembered {
        return Err("请选择导出目录或使用本次导出结果中的位置".into());
    }
    let root = storage::configured_root().map_err(|e| e.to_string())?;
    let path = validate_output(&root, &path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe")
            .arg(path)
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| "无法打开导出目录")?;
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

#[tauri::command]
pub(crate) async fn choose_export_directory(
    app: tauri::AppHandle,
) -> std::result::Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file().set_title("选择导出目录");
        if let Some(path) =
            remembered(&settings_path().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?
        {
            storage::check_path(&path).map_err(|e| e.to_string())?;
            if path.is_dir() {
                dialog = dialog.set_directory(path);
            }
        }
        let Some(folder) = dialog.blocking_pick_folder() else {
            return Ok(None);
        };
        let path = folder
            .into_path()
            .map_err(|_| "请选择本机导出目录".to_owned())?;
        let access = app.state::<StorageAccess>();
        let _guard = access
            .0
            .try_lock()
            .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
        let root = storage::configured_root().map_err(|e| e.to_string())?;
        let path = validate_output(&root, &path).map_err(|e| e.to_string())?;
        remember(&settings_path().map_err(|e| e.to_string())?, &path).map_err(|e| e.to_string())?;
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|_| "无法选择导出目录".to_owned())?
}
#[tauri::command]
pub(crate) fn get_export_status(
    access: tauri::State<'_, ExportAccess>,
) -> std::result::Result<Progress, String> {
    access
        .progress
        .lock()
        .map(|p| p.clone())
        .map_err(|_| "导出状态暂不可用".into())
}
#[tauri::command]
pub(crate) fn stop_export(access: tauri::State<'_, ExportAccess>) {
    access.cancel.store(true, Ordering::Relaxed);
}

#[tauri::command]
pub(crate) fn start_export(
    app: tauri::AppHandle,
    options: Options,
) -> std::result::Result<Progress, String> {
    options.validate().map_err(|e| e.to_string())?;
    let access = app.state::<ExportAccess>();
    let mut progress = access
        .progress
        .lock()
        .map_err(|_| "导出状态暂不可用".to_owned())?;
    if progress.running {
        return Err("导出任务正在进行".into());
    }
    // Synchronous preflight gives callers an immediate busy/invalid-path error.
    let storage = app.state::<StorageAccess>();
    let guard = storage
        .0
        .try_lock()
        .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
    let root = storage::configured_root().map_err(|e| e.to_string())?;
    let login = app.state::<LoginAccess>();
    if login
        .0
        .try_lock()
        .map_err(|_| "登录操作正在进行".to_owned())?
        .is_pending()
    {
        return Err("登录操作正在进行".into());
    }
    let path = settings_path().map_err(|e| e.to_string())?;
    let output = match &options.output_directory {
        Some(directory) => directory.clone(),
        None => remembered(&path)
            .map_err(|e| e.to_string())?
            .ok_or("请先选择导出目录")?,
    };
    let output = validate_output(&root, &output).map_err(|e| e.to_string())?;
    remember(&path, &output).map_err(|e| e.to_string())?;
    access.cancel.store(false, Ordering::Relaxed);
    *progress = Progress {
        running: true,
        stage: "准备导出".into(),
        started_at: now(),
        ..Progress::default()
    };
    let initial = progress.clone();
    drop(progress);
    drop(guard);
    tauri::async_runtime::spawn_blocking(move || {
        let storage = app.state::<StorageAccess>();
        let export = app.state::<ExportAccess>();
        let execute = with_storage_lock(&storage, || -> std::result::Result<(), String> {
            // A directory switch can win the preflight/worker race: refuse to export a different DB.
            if storage::configured_root().map_err(|e| e.to_string())? != root {
                return Err("数据目录已改变，请重新开始导出".into());
            }
            if app
                .state::<LoginAccess>()
                .0
                .try_lock()
                .map_err(|_| "登录操作正在进行".to_owned())?
                .is_pending()
            {
                return Err("登录操作正在进行".into());
            }
            run(&root, &output, &options, &export.cancel, |p| {
                if let Ok(mut status) = export.progress.lock() {
                    *status = p.clone();
                }
                let _ = app.emit("export-progress", &p);
            });
            Ok(())
        });
        if let Err(e) = execute {
            if let Ok(mut p) = export.progress.lock() {
                p.running = false;
                p.stage = "导出未完成".into();
                p.outcome = "incomplete".into();
                p.error = Some(e);
                p.finished_at = Some(now());
                let _ = app.emit("export-progress", &*p);
            }
        }
    });
    Ok(initial)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remembered_directory_roundtrip_without_global_settings() {
        let d = super::super::tests::fixture_directory();
        let file = d.path().join("settings/export-directory.json");
        assert!(remembered(&file).unwrap().is_none());
        remember(&file, d.path()).unwrap();
        assert_eq!(remembered(&file).unwrap().as_deref(), Some(d.path()));
        let canonical = d.path().canonicalize().unwrap();
        remember(&file, &canonical).unwrap();
        let saved: PathBuf = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(saved, storage::friendly_path(&canonical));
        assert!(!saved.to_string_lossy().starts_with(r"\\?\"));
    }
}
