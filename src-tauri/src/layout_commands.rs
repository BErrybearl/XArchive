use super::*;

#[derive(Default)]
pub(crate) struct LayoutAccess {
    cancel: AtomicBool,
    progress: Mutex<storage::LayoutProgress>,
}
#[tauri::command]
pub(crate) fn get_layout_status(
    access: tauri::State<'_, LayoutAccess>,
) -> Result<storage::LayoutProgress, String> {
    access
        .progress
        .lock()
        .map(|p| p.clone())
        .map_err(|_| "整理状态暂不可用".into())
}
#[tauri::command]
pub(crate) fn stop_layout(access: tauri::State<'_, LayoutAccess>) {
    access.cancel.store(true, Ordering::Relaxed);
}
#[tauri::command]
pub(crate) async fn organize_media(
    app: tauri::AppHandle,
    layout: storage::Layout,
    directory: String,
) -> Result<storage::LayoutProgress, String> {
    {
        let access = app.state::<LayoutAccess>();
        let mut p = access.progress.lock().map_err(|_| "整理状态暂不可用")?;
        if p.running {
            return Err("整理任务正在进行".into());
        }
        access.cancel.store(false, Ordering::Relaxed);
        *p = storage::LayoutProgress {
            running: true,
            layout,
            stage: "准备整理".into(),
            ..Default::default()
        };
    }
    let worker = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let access = worker.state::<StorageAccess>();
        let _guard = access.try_acquire()?;
        if worker
            .state::<LoginAccess>()
            .0
            .try_lock()
            .map_err(|_| "登录操作正在进行")?
            .is_pending()
        {
            return Err(auth::AuthError::Busy.to_string());
        }
        let root = storage::configured_root().map_err(|e| e.to_string())?;
        if storage::friendly_path(&root) != PathBuf::from(directory) {
            return Err("数据目录已改变，请重新确认整理位置".into());
        }
        let mut store = storage::Store::open(&root).map_err(|e| e.to_string())?;
        let task = worker.state::<LayoutAccess>();
        Ok(store.organize_media(layout, &task.cancel, |p| {
            if let Ok(mut current) = task.progress.lock() {
                *current = p.clone();
            }
            let _ = worker.emit("layout-progress", &p);
        }))
    })
    .await
    .map_err(|_| "整理线程未完成；下次启动将恢复重命名日志".to_owned())
    .and_then(|r| r);
    if let Err(e) = &result {
        if let Ok(mut p) = app.state::<LayoutAccess>().progress.lock() {
            p.running = false;
            p.stage = "整理未完成".into();
            p.error = Some(e.clone());
            let _ = app.emit("layout-progress", &*p);
        }
    }
    result
}
