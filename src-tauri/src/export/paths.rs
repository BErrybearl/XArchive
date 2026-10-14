use super::*;
use std::{collections::HashSet, path::Component};
pub(super) fn invalid(message: &str) -> StorageError {
    StorageError::Invalid(message.into())
}

pub(super) fn resolved(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
        || path
            .as_os_str()
            .to_string_lossy()
            .split(['\\', '/'])
            .any(|p| p == "." || p == "..")
    {
        return Err(invalid("请选择完整路径，导出路径不能包含越界部分"));
    }
    storage::check_path(path)?;
    // Resolve the existing ancestor to catch casing and aliases before creation.
    let mut base = path;
    let mut missing = Vec::new();
    while !base.exists() {
        missing.push(
            base.file_name()
                .ok_or_else(|| invalid("导出路径无效"))?
                .to_owned(),
        );
        base = base.parent().ok_or_else(|| invalid("导出路径无效"))?;
    }
    let mut result = base.canonicalize()?;
    for part in missing.into_iter().rev() {
        result.push(part);
    }
    Ok(result)
}
fn inside(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        // Missing suffix components have not yet been canonicalized; Windows is case insensitive.
        let p = path.to_string_lossy().to_lowercase();
        let r = root
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase();
        p == r
            || p.strip_prefix(&r)
                .is_some_and(|s| s.starts_with(['\\', '/']))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}
pub(super) fn validate_directory(root: &Path, path: &Path) -> Result<()> {
    let path = resolved(path)?;
    let data = resolved(root)?;
    if inside(&path, &data) {
        return Err(invalid("导出目录不得位于数据目录内部"));
    }
    Ok(())
}
pub(super) fn validate_output(root: &Path, output: &Path) -> Result<PathBuf> {
    validate_directory(root, output)?;
    if !output.is_dir() {
        return Err(invalid("请选择已存在的导出目录"));
    }
    Ok(storage::friendly_path(&output.canonicalize()?))
}
pub(super) fn clean_filename(raw: &str) -> String {
    let mut s = String::new();
    let mut units = 0;
    for c in raw.chars() {
        let c = if c.is_control() || "<>:\"/\\|?*".contains(c) {
            '_'
        } else {
            c
        };
        units += c.len_utf16();
        if units > 80 {
            break;
        }
        s.push(c);
    }
    while s.contains("..") {
        s = s.replace("..", "_");
    }
    s = s.trim_matches([' ', '.']).to_owned();
    if s.is_empty() {
        s = "unnamed".into();
    }
    if reserved_name(&s) {
        s.insert(0, '_');
    }
    s
}
fn reserved_name(raw: &str) -> bool {
    let stem = raw
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
        || ["COM", "LPT"].iter().any(|p| {
            stem.strip_prefix(p).is_some_and(|n| {
                ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"].contains(&n)
            })
        })
}
pub(super) fn export_path(directory: &Path, relative: &str) -> Result<PathBuf> {
    let name = relative
        .strip_prefix("media/")
        .ok_or_else(|| invalid("导出清单路径越界"))?;
    // File names are assembled from several individually bounded components.
    if name.is_empty()
        || name.encode_utf16().count() > 240
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || name.contains("..")
        || name.ends_with([' ', '.'])
        || reserved_name(name)
    {
        return Err(invalid("导出清单包含非法或越界文件名"));
    }
    storage::media_path(directory, relative)
}
pub(super) fn account_folder(store: &Store, account: &str, _screen: &str) -> Result<String> {
    // Allocate deterministically across all archived targets, including case/cleaning collisions.
    let mut accounts = storage::list_archived_accounts(store.root())?;
    accounts.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    let mut used = HashSet::new();
    for a in accounts {
        let base = clean_filename(&a.screen_name);
        let mut name = base.clone();
        let mut suffix = 1u64;
        while !used.insert(name.to_lowercase()) {
            name = format!("{base}_{suffix}");
            suffix += 1;
        }
        if a.user_id == account {
            return Ok(name);
        }
    }
    Err(invalid("导出账号缺失"))
}
pub(super) fn extension(entity: &Entity) -> String {
    let ext = match entity.mime.as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/avif" => "avif",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        _ => "",
    };
    if !ext.is_empty() {
        return ext.into();
    }
    if let Some(ext) = entity.sources[0]
        .original_filename
        .rsplit_once('.')
        .map(|(_, e)| e)
    {
        if !ext.is_empty() && ext.len() <= 10 && ext.bytes().all(|c| c.is_ascii_alphanumeric()) {
            return ext.to_ascii_lowercase();
        }
    }
    "bin".into()
}
