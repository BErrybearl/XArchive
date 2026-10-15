use super::*;

/// Held for the entire GUI session or CLI task, independently of SQLite transactions.
/// The file remains on disk; only the live OS handle denotes occupancy.
pub(crate) struct DirectoryLock {
    root: PathBuf,
    _file: File,
}

impl DirectoryLock {
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        check_path(root)?;
        fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        let path = root.join(".xarchive.lock");
        check_path(&path)?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options.open(path).map_err(|error| {
            if matches!(error.raw_os_error(), Some(32 | 33)) {
                invalid("数据目录已被其他 XArchive 界面或命令行实例打开，请关闭后重试")
            } else {
                error.into()
            }
        })?;
        #[cfg(not(windows))]
        file.try_lock()
            .map_err(|_| invalid("数据目录已被其他 XArchive 实例打开，请关闭后重试"))?;
        Ok(Self { root, _file: file })
    }

    pub(crate) fn holds(&self, root: &Path) -> Result<bool> {
        check_path(root)?;
        Ok(self.root == root.canonicalize()?)
    }

    /// Keep the current directory protected unless acquiring and persisting the new location succeed.
    pub(crate) fn switch<T>(
        &mut self,
        root: &Path,
        persist: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if self.holds(root)? {
            return persist();
        }
        let next = Self::acquire(root)?;
        let result = persist()?;
        *self = next;
        Ok(result)
    }
}
