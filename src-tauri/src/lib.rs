use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

mod archive;
mod auth;
pub mod cli;
mod export;
mod layout_commands;
mod legacy;
mod licenses;
mod platform;
pub mod storage;
#[cfg(all(test, windows))]
mod test_support;

struct StorageAccess(Mutex<()>);
struct DirectoryAccess(Mutex<storage::DirectoryLock>);
impl StorageAccess {
    fn try_acquire(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        self.0.try_lock().map_err(|_| "数据操作正在进行".to_owned())
    }
}
#[derive(Default)]
struct ArchiveAccess {
    cancel: AtomicBool,
    progress: Mutex<archive::Progress>,
}
#[tauri::command]
fn get_archive_settings() -> Result<archive::Settings, String> {
    let fallback = archive::load_settings(&archive::settings_path().map_err(|e| e.to_string())?)?;
    let root = storage::configured_root().map_err(|e| e.to_string())?;
    if !storage::status(&root)
        .map_err(|e| e.to_string())?
        .initialized
    {
        return Ok(fallback);
    }
    storage::Store::open_readonly(&root)
        .and_then(|s| s.workspace_settings(&fallback))
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn save_archive_settings(
    app: tauri::AppHandle,
    settings: archive::Settings,
) -> Result<(), String> {
    with_account_store(app, move |s| {
        s.save_account_settings(&settings)?;
        if settings.preferences.validate().is_ok() {
            archive::save_settings(&archive::settings_path()?, &settings)
                .map_err(storage::StorageError::Invalid)?;
        }
        Ok(())
    })
    .await
}
#[tauri::command]
fn list_archived_accounts() -> Result<Vec<storage::ArchivedAccount>, String> {
    storage::list_archived_accounts(&storage::configured_root().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn get_network_route() -> Result<String, String> {
    let root = storage::configured_root().map_err(|e| e.to_string())?;
    let proxy = auth::platform_proxy(&root).map_err(|e| e.to_string())?;
    let plan = platform::proxy::ProxyPlan::resolve(&proxy).map_err(|e| e.message.to_string())?;
    let url = url::Url::parse("https://x.com/").expect("static URL");
    Ok(if plan.endpoint(&url).is_none() {
        "直连"
    } else if proxy.is_empty() {
        "系统代理"
    } else {
        "代理"
    }
    .into())
}
#[tauri::command]
fn save_proxy(proxy: String, access: tauri::State<'_, StorageAccess>) -> Result<(), String> {
    let _guard = access.0.try_lock().map_err(|_| "数据操作正在进行")?;
    auth::save_proxy(
        &storage::configured_root().map_err(|e| e.to_string())?,
        &proxy,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_archive_status(
    access: tauri::State<'_, ArchiveAccess>,
) -> Result<archive::Progress, String> {
    access
        .progress
        .lock()
        .map(|p| p.clone())
        .map_err(|_| "归档状态暂不可用".into())
}
#[tauri::command]
fn stop_archive(access: tauri::State<'_, ArchiveAccess>) {
    access.cancel.store(true, Ordering::Relaxed);
}
#[tauri::command]
async fn archive_account(
    app: tauri::AppHandle,
    options: archive::Options,
) -> Result<archive::Progress, String> {
    options.validate()?;
    {
        let access = app.state::<ArchiveAccess>();
        let mut p = access
            .progress
            .lock()
            .map_err(|_| "归档状态暂不可用".to_owned())?;
        if p.running {
            return Err("归档任务正在进行".into());
        }
        access.cancel.store(false, Ordering::Relaxed);
        *p = archive::Progress {
            running: true,
            stage: "准备归档".into(),
            target: Some(auth::Account {
                user_id: String::new(),
                screen_name: options.target.trim_start_matches('@').to_owned(),
                display_name: String::new(),
            }),
            ..archive::Progress::default()
        };
    }
    let worker = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let access = worker.state::<StorageAccess>();
        let _guard = access
            .0
            .try_lock()
            .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
        let login = worker.state::<LoginAccess>();
        if login
            .0
            .try_lock()
            .map_err(|_| "登录操作正在进行".to_owned())?
            .is_pending()
        {
            return Err(auth::AuthError::Busy.to_string());
        }
        let platform = worker.state::<PlatformAccess>();
        let mut session = platform
            .0
            .try_lock()
            .map_err(|_| "接口操作正在进行".to_owned())?;
        let root = storage::configured_root().map_err(|_| "数据目录暂不可用".to_owned())?;
        let archive = worker.state::<ArchiveAccess>();
        Ok(archive::run(
            &root,
            &mut session,
            &options,
            &archive.cancel,
            |p| {
                if let Ok(mut current) = archive.progress.lock() {
                    *current = p.clone();
                }
                let _ = worker.emit("archive-progress", &p);
            },
        ))
    })
    .await
    .map_err(|_| "归档线程未完成；已保存的数据和检查点保留".to_owned())
    .and_then(|r| r);
    let access = app.state::<ArchiveAccess>();
    if result.is_err() {
        if let Ok(mut p) = access.progress.lock() {
            p.running = false;
            p.stage = "归档未完成".into();
            p.outcome = "incomplete".into();
            p.summary = "归档未完成；已保存的数据和检查点保留".into();
            let _ = app.emit("archive-progress", &*p);
        }
    }
    result
}

#[derive(Default)]
struct ImportAccess {
    cancel: AtomicBool,
    progress: Mutex<legacy::Progress>,
    report: Mutex<Option<legacy::Report>>,
}
#[derive(Serialize)]
struct ImportStatus {
    progress: legacy::Progress,
    report: Option<legacy::Report>,
}
#[derive(Serialize)]
struct ImportSelection {
    path: String,
    destination: String,
}

struct LoginAccess(Mutex<auth::LoginManager>);
struct PlatformAccess(Mutex<platform::Session>);

fn require_idle(archive_running: bool, import_running: bool) -> Result<(), String> {
    if archive_running || import_running {
        Err("任务正在进行，请结束归档或导入后再管理数据".into())
    } else {
        Ok(())
    }
}
async fn with_account_store<T: Send + 'static>(
    app: tauri::AppHandle,
    action: impl FnOnce(&mut storage::Store) -> storage::Result<T> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let _guard = access
            .0
            .try_lock()
            .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
        require_idle(
            app.state::<ArchiveAccess>()
                .progress
                .lock()
                .map_err(|_| "归档状态暂不可用")?
                .running,
            app.state::<ImportAccess>()
                .progress
                .lock()
                .map_err(|_| "导入状态暂不可用")?
                .running,
        )?;
        let root = storage::configured_root().map_err(|e| e.to_string())?;
        let mut store = storage::Store::open(&root).map_err(|e| e.to_string())?;
        action(&mut store).map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "数据操作未完成".to_owned())?
}
fn account_store_read() -> Result<storage::Store, String> {
    storage::Store::open_readonly(&storage::configured_root().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn add_account(app: tauri::AppHandle, account: String) -> Result<archive::Settings, String> {
    let fallback = archive::load_settings(&archive::settings_path().map_err(|e| e.to_string())?)?;
    with_account_store(app, move |s| s.add_account(&account, &fallback)).await
}
#[tauri::command]
fn get_account_settings(account: String) -> Result<archive::Settings, String> {
    let fallback = archive::load_settings(&archive::settings_path().map_err(|e| e.to_string())?)?;
    account_store_read()?
        .account_settings(&account, &fallback)
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn get_account_data(account: String) -> Result<Vec<storage::AccountDataRow>, String> {
    account_store_read()?
        .account_data(&account)
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn preview_account_delete(
    app: tauri::AppHandle,
    account: String,
    scope: storage::DeleteScope,
) -> Result<storage::DeletePreview, String> {
    with_account_store(app, move |s| s.preview_account_delete(&account, scope)).await
}
#[tauri::command]
async fn delete_account_data(
    app: tauri::AppHandle,
    account: String,
    scope: storage::DeleteScope,
    options: storage::DeleteOptions,
) -> Result<storage::DeleteResult, String> {
    if options.preview_token.is_none() {
        return Err("请先预览账号数据，再确认删除".into());
    }
    with_account_store(app, move |s| {
        s.delete_account_data(&account, scope, &options)
    })
    .await
}
#[tauri::command]
async fn inspect_account(
    app: tauri::AppHandle,
    account: String,
) -> Result<storage::IntegrityReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let _guard = access
            .0
            .try_lock()
            .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
        require_idle(
            app.state::<ArchiveAccess>()
                .progress
                .lock()
                .map_err(|_| "归档状态暂不可用")?
                .running,
            app.state::<ImportAccess>()
                .progress
                .lock()
                .map_err(|_| "导入状态暂不可用")?
                .running,
        )?;
        account_store_read()?
            .inspect_account(&account)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "校验未完成".to_owned())?
}
#[tauri::command]
async fn reset_account_progress(app: tauri::AppHandle, account: String) -> Result<(), String> {
    with_account_store(app, move |s| s.reset_account_progress(&account)).await
}
#[tauri::command]
fn open_media_directory(account: Option<String>) -> Result<(), String> {
    let root = storage::configured_root().map_err(|e| e.to_string())?;
    let store = storage::Store::open_readonly(&root).map_err(|e| e.to_string())?;
    let path = store
        .media_directory(account.as_deref())
        .map_err(|e| e.to_string())?;
    storage::check_path(&path).map_err(|e| e.to_string())?;
    if !path.is_dir() {
        return Err("媒体目录尚未创建".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe")
            .arg(storage::friendly_path(&path))
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| "无法打开媒体目录")?;
    }
    Ok(())
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    #[test]
    fn writes_refuse_running_archive_or_import() {
        assert!(require_idle(true, false).is_err());
        assert!(require_idle(false, true).is_err());
        assert!(require_idle(false, false).is_ok());
        let access = StorageAccess(Mutex::new(()));
        let _guard = access.0.lock().unwrap();
        assert!(access.0.try_lock().is_err());
    }
    #[test]
    fn login_status_returns_while_storage_is_held() {
        let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let base = project.join(".runtime/login-lock-tests");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        let storage = StorageAccess(Mutex::new(()));
        let _guard = storage.0.lock().unwrap();
        assert_eq!(storage.try_acquire().err().unwrap(), "数据操作正在进行");
        let login = LoginAccess(Mutex::new(auth::LoginManager::default()));
        let start = std::time::Instant::now();
        assert!(!read_login_status(&login, dir.path()).unwrap().saved);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        let _login_guard = login.0.lock().unwrap();
        assert_eq!(
            read_login_status(&login, dir.path()).err().unwrap(),
            "登录操作正在进行"
        );
    }
    #[test]
    fn initialization_and_migrations_run_on_a_background_thread() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".runtime/initialization-tests");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        let initiating_thread = std::thread::current().id();
        let status = tauri::async_runtime::block_on(initialize_in_background(move || {
            assert_ne!(std::thread::current().id(), initiating_thread);
            let store = storage::Store::open(dir.path()).map_err(|e| e.to_string())?;
            drop(store);
            storage::status(dir.path()).map_err(|e| e.to_string())
        }))
        .unwrap();
        assert!(status.initialized);
    }
}

#[derive(Serialize)]
struct ProgramInfo {
    name: &'static str,
    version: &'static str,
    development: bool,
}

#[tauri::command]
fn get_program_info() -> ProgramInfo {
    ProgramInfo {
        name: "XArchive",
        version: env!("CARGO_PKG_VERSION"),
        development: cfg!(debug_assertions),
    }
}

#[tauri::command]
async fn get_license_notices(section: usize) -> Result<licenses::LicenseNotices, String> {
    tauri::async_runtime::spawn_blocking(move || licenses::read(section))
        .await
        .map_err(|_| "无法读取内嵌许可声明".to_owned())?
}

#[tauri::command]
fn get_storage_status() -> Result<storage::StorageStatus, String> {
    let root = storage::configured_root().map_err(|error| error.to_string())?;
    storage::status(&root).map_err(|error| error.to_string())
}

#[tauri::command]
async fn initialize_storage(app: tauri::AppHandle) -> Result<storage::StorageStatus, String> {
    initialize_in_background(move || {
        let access = app.state::<StorageAccess>();
        let _guard = access
            .0
            .try_lock()
            .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
        let root = storage::configured_root().map_err(|error| error.to_string())?;
        let store = storage::Store::open(&root).map_err(|error| error.to_string())?;
        drop(store);
        storage::status(&root).map_err(|error| error.to_string())
    })
    .await
}

async fn initialize_in_background(
    task: impl FnOnce() -> Result<storage::StorageStatus, String> + Send + 'static,
) -> Result<storage::StorageStatus, String> {
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|_| "数据初始化未完成".to_owned())?
}

#[tauri::command]
async fn choose_data_directory(
    app: tauri::AppHandle,
    access: tauri::State<'_, StorageAccess>,
    login: tauri::State<'_, LoginAccess>,
    directory: tauri::State<'_, DirectoryAccess>,
) -> Result<Option<storage::StorageStatus>, String> {
    if login
        .0
        .lock()
        .map_err(|_| "登录操作暂不可用".to_owned())?
        .is_pending()
    {
        return Err(auth::AuthError::Busy.to_string());
    }
    let folder = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("选择 XArchive 数据目录（不会搬移已有数据）")
            .blocking_pick_folder()
    })
    .await
    .map_err(|_| "无法打开目录选择窗口".to_owned())?;
    let Some(folder) = folder else {
        return Ok(None);
    };
    let path = folder
        .into_path()
        .map_err(|_| "请选择本机目录".to_owned())?;
    let _guard = access
        .0
        .try_lock()
        .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
    if login
        .0
        .lock()
        .map_err(|_| "登录操作暂不可用".to_owned())?
        .is_pending()
    {
        return Err(auth::AuthError::Busy.to_string());
    }
    let mut directory = directory.0.lock().map_err(|_| "数据目录锁暂不可用")?;
    directory
        .switch(&path, || storage::select_location(&path))
        .map(Some)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_login_status(app: tauri::AppHandle) -> Result<auth::LoginStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = storage::configured_root().map_err(|error| error.to_string())?;
        read_login_status(&app.state::<LoginAccess>(), &root)
    })
    .await
    .map_err(|_| "无法读取登录状态".to_owned())?
}

fn read_login_status(
    access: &LoginAccess,
    root: &std::path::Path,
) -> Result<auth::LoginStatus, String> {
    access
        .0
        .try_lock()
        .map_err(|_| "登录操作正在进行".to_owned())?
        .status(root)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn begin_login(app: tauri::AppHandle, proxy: String) -> Result<auth::LoginStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let _guard = access.try_acquire()?;
        let root = storage::configured_root().map_err(|error| error.to_string())?;
        app.state::<LoginAccess>()
            .0
            .lock()
            .map_err(|_| "登录操作暂不可用".to_owned())?
            .begin(&root, &proxy)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "无法启动登录操作".to_owned())?
}

#[tauri::command]
async fn confirm_login(app: tauri::AppHandle) -> Result<auth::LoginStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let _guard = access.try_acquire()?;
        let root = storage::configured_root().map_err(|error| error.to_string())?;
        let platform = app.state::<PlatformAccess>();
        let mut session = platform
            .0
            .lock()
            .map_err(|_| "接口操作暂不可用".to_owned())?;
        app.state::<LoginAccess>()
            .0
            .lock()
            .map_err(|_| "登录操作暂不可用".to_owned())?
            .confirm(&root, &mut session)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "无法完成登录检查".to_owned())?
}

#[tauri::command]
async fn manual_login(
    app: tauri::AppHandle,
    proxy: String,
    auth_token: String,
    ct0: String,
) -> Result<auth::LoginStatus, String> {
    // Wrap immediately; credentials never enter a response or a diagnostic.
    let auth_token = zeroize::Zeroizing::new(auth_token);
    let ct0 = zeroize::Zeroizing::new(ct0);
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let _guard = access.try_acquire()?;
        let root = storage::configured_root().map_err(|error| error.to_string())?;
        let platform = app.state::<PlatformAccess>();
        let mut session = platform
            .0
            .lock()
            .map_err(|_| "接口操作暂不可用".to_owned())?;
        app.state::<LoginAccess>()
            .0
            .lock()
            .map_err(|_| "登录操作暂不可用".to_owned())?
            .manual(&root, &proxy, auth_token, ct0, &mut session)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "无法完成登录检查".to_owned())?
}

#[tauri::command]
async fn cancel_login(app: tauri::AppHandle) -> Result<auth::LoginStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let _guard = access.try_acquire()?;
        let root = storage::configured_root().map_err(|error| error.to_string())?;
        app.state::<LoginAccess>()
            .0
            .lock()
            .map_err(|_| "登录操作暂不可用".to_owned())?
            .cancel(&root)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "无法取消登录操作".to_owned())?
}

#[tauri::command]
async fn clear_login_browser_profile(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let _guard = access.try_acquire()?;
        let root = storage::configured_root().map_err(|error| error.to_string())?;
        app.state::<LoginAccess>()
            .0
            .lock()
            .map_err(|_| "登录操作暂不可用".to_owned())?
            .clear_browser_profile(&root)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "无法清除登录浏览器资料".to_owned())?
}

#[tauri::command]
async fn diagnose_connection(
    app: tauri::AppHandle,
    target: String,
    include_timeline: bool,
) -> platform::DiagnosticReport {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let Ok(_guard) = access.0.try_lock() else {
            return platform::DiagnosticReport::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        };
        let login = app.state::<LoginAccess>();
        let Ok(login) = login.0.try_lock() else {
            return platform::DiagnosticReport::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        };
        if login.is_pending() {
            return platform::DiagnosticReport::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        }
        drop(login);
        let platform = app.state::<PlatformAccess>();
        let Ok(mut session) = platform.0.try_lock() else {
            return platform::DiagnosticReport::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        };
        match storage::configured_root() {
            Ok(root) => platform::diagnose(&root, &mut session, &target, include_timeline),
            Err(_) => platform::DiagnosticReport::failure(platform::PlatformError::local()),
        }
    })
    .await
    .unwrap_or_else(|_| platform::DiagnosticReport::failure(platform::PlatformError::local()))
}
#[tauri::command]
async fn probe_platform(app: tauri::AppHandle, target: String) -> platform::ProbeResult {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let Ok(_guard) = access.0.try_lock() else {
            return platform::ProbeResult::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        };
        let login = app.state::<LoginAccess>();
        let Ok(login) = login.0.try_lock() else {
            return platform::ProbeResult::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        };
        if login.is_pending() {
            return platform::ProbeResult::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        }
        drop(login);
        let platform = app.state::<PlatformAccess>();
        let Ok(mut session) = platform.0.try_lock() else {
            return platform::ProbeResult::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        };
        match storage::configured_root() {
            Ok(root) => platform::probe(&root, &mut session, &target),
            Err(_) => platform::ProbeResult::failure(platform::PlatformError::local()),
        }
    })
    .await
    .unwrap_or_else(|_| platform::ProbeResult::failure(platform::PlatformError::local()))
}

#[tauri::command]
async fn refresh_platform_params(app: tauri::AppHandle) -> platform::RefreshResult {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let Ok(_guard) = access.0.try_lock() else {
            return platform::RefreshResult::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        };
        let login = app.state::<LoginAccess>();
        let Ok(login) = login.0.try_lock() else {
            return platform::RefreshResult::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        };
        if login.is_pending() {
            return platform::RefreshResult::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        }
        drop(login);
        let platform = app.state::<PlatformAccess>();
        let Ok(mut session) = platform.0.try_lock() else {
            return platform::RefreshResult::failure(platform::PlatformError::new(
                platform::Category::Busy,
            ));
        };
        match storage::configured_root() {
            Ok(root) => platform::refresh(&root, &mut session),
            Err(_) => platform::RefreshResult::failure(platform::PlatformError::local()),
        }
    })
    .await
    .unwrap_or_else(|_| platform::RefreshResult::failure(platform::PlatformError::local()))
}

#[tauri::command]
async fn choose_legacy_file(app: tauri::AppHandle) -> Result<Option<ImportSelection>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let access = app.state::<StorageAccess>();
        let _guard = access
            .0
            .try_lock()
            .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
        let login = app.state::<LoginAccess>();
        if login
            .0
            .lock()
            .map_err(|_| "登录操作暂不可用".to_owned())?
            .is_pending()
        {
            return Err(auth::AuthError::Busy.to_string());
        }
        let Some(file) = app
            .dialog()
            .file()
            .set_title("选择旧版 SQLite 库的隔离副本")
            .add_filter("SQLite 数据库", &["db", "sqlite", "sqlite3"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = file
            .into_path()
            .map_err(|_| "请选择本机旧库文件".to_owned())?;
        let destination = storage::configured_root().map_err(|e| e.to_string())?;
        Ok(Some(ImportSelection {
            path: storage::friendly_path(&path).to_string_lossy().into_owned(),
            destination: storage::friendly_path(&destination)
                .to_string_lossy()
                .into_owned(),
        }))
    })
    .await
    .map_err(|_| "无法选择旧库文件".to_owned())?
}

#[tauri::command]
fn get_legacy_import_status(
    access: tauri::State<'_, ImportAccess>,
) -> Result<ImportStatus, String> {
    Ok(ImportStatus {
        progress: access
            .progress
            .lock()
            .map_err(|_| "无法读取导入进度".to_owned())?
            .clone(),
        report: access
            .report
            .lock()
            .map_err(|_| "无法读取导入报告".to_owned())?
            .clone(),
    })
}

#[tauri::command]
fn stop_legacy_import(access: tauri::State<'_, ImportAccess>) {
    access.cancel.store(true, Ordering::Relaxed);
}

#[tauri::command]
async fn import_legacy(
    app: tauri::AppHandle,
    path: String,
    destination: String,
) -> Result<legacy::Report, String> {
    {
        let import = app.state::<ImportAccess>();
        let mut progress = import
            .progress
            .lock()
            .map_err(|_| "导入状态暂不可用".to_owned())?;
        if progress.running {
            return Err("旧库导入正在进行".to_owned());
        }
        import.cancel.store(false, Ordering::Relaxed);
        *import
            .report
            .lock()
            .map_err(|_| "导入状态暂不可用".to_owned())? = None;
        *progress = legacy::Progress {
            running: true,
            stage: "准备导入".into(),
            done: 0,
            total: 0,
        };
    }
    let worker = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let access = worker.state::<StorageAccess>();
        let _guard = access
            .0
            .try_lock()
            .map_err(|_| "数据操作正在进行，请稍后重试".to_owned())?;
        let login = worker.state::<LoginAccess>();
        if login
            .0
            .lock()
            .map_err(|_| "登录操作暂不可用".to_owned())?
            .is_pending()
        {
            return Err(auth::AuthError::Busy.to_string());
        }
        let root = storage::configured_root().map_err(|e| e.to_string())?;
        if root != PathBuf::from(destination) {
            return Err("数据位置已改变，请重新选择旧库并确认导入位置".to_owned());
        }
        let import = worker.state::<ImportAccess>();
        legacy::import(&root, &PathBuf::from(path), &import.cancel, |p| {
            if let Ok(mut status) = import.progress.lock() {
                *status = legacy::Progress { running: true, ..p };
            }
        })
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "导入线程未完成，核对未通过；已提交数据保留，可重新导入".to_owned())
    .and_then(|r| r);
    let import = app.state::<ImportAccess>();
    {
        let mut progress = import
            .progress
            .lock()
            .map_err(|_| "无法保存导入结果状态".to_owned())?;
        if let Ok(report) = &result {
            *import
                .report
                .lock()
                .map_err(|_| "无法保存导入结果状态".to_owned())? = Some(report.clone());
        }
        progress.running = false;
        if result.is_err() {
            progress.stage = "核对未通过".into();
        }
    }
    result
}

pub fn run() {
    // Acquire before building Tauri: an occupied directory must never be opened by a window.
    let directory = storage::configured_root()
        .and_then(|root| storage::DirectoryLock::acquire(&root))
        .expect("无法打开 XArchive 数据目录");
    let commands: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
        layout_commands::get_layout_status,
        layout_commands::organize_media,
        layout_commands::stop_layout,
        export::commands::choose_export_directory,
        export::commands::get_export_directory,
        export::commands::open_export_directory,
        export::commands::start_export,
        export::commands::get_export_status,
        export::commands::stop_export,
        archive_account,
        add_account,
        get_account_settings,
        get_account_data,
        preview_account_delete,
        delete_account_data,
        inspect_account,
        reset_account_progress,
        open_media_directory,
        get_archive_settings,
        save_archive_settings,
        list_archived_accounts,
        save_proxy,
        get_network_route,
        get_archive_status,
        stop_archive,
        get_program_info,
        get_license_notices,
        get_storage_status,
        initialize_storage,
        choose_data_directory,
        get_login_status,
        begin_login,
        confirm_login,
        manual_login,
        cancel_login,
        clear_login_browser_profile,
        diagnose_connection,
        probe_platform,
        refresh_platform_params,
        choose_legacy_file,
        import_legacy,
        get_legacy_import_status,
        stop_legacy_import
    ];
    tauri::Builder::default()
        .manage(DirectoryAccess(Mutex::new(directory)))
        .manage(StorageAccess(Mutex::new(())))
        .manage(layout_commands::LayoutAccess::default())
        .manage(ImportAccess::default())
        .manage(ArchiveAccess::default())
        .manage(export::ExportAccess::default())
        .manage(PlatformAccess(Mutex::new(platform::Session::default())))
        .manage(LoginAccess(Mutex::new(auth::LoginManager::default())))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            if let Ok(root) = storage::configured_root() {
                if root.join("archive.sqlite3").exists() {
                    storage::Store::open(&root)?;
                }
                if let Ok(mut login) = app.state::<LoginAccess>().0.lock() {
                    login.retry_cleanup(&root);
                }
            }
            Ok(())
        })
        .invoke_handler(move |invoke| {
            // Capabilities contain no remote grant. Also restrict every application command to main.
            if invoke.message.webview_ref().label() != "main" {
                invoke.resolver.reject("此窗口无应用命令权限");
                return true;
            }
            commands(invoke)
        })
        .build(tauri::generate_context!())
        .expect("unable to start XArchive")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                if let Ok(root) = storage::configured_root() {
                    if let Ok(mut login) = app.state::<LoginAccess>().0.lock() {
                        let _ = login.cancel(&root);
                    }
                }
            }
        });
}
