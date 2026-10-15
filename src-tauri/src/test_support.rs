//! Win32 fixtures shared by unit tests; no command interpreter is needed.
use std::{
    fs, io,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::GENERIC_WRITE,
    Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT},
    System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_REPARSE_POINT},
};

pub(crate) fn junction(link: &Path, target: &Path) -> io::Result<()> {
    // Both ends must stay inside isolated test data, including canonical parents.
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime")
        .canonicalize()?;
    let target = target.canonicalize()?;
    let parent = link
        .parent()
        .ok_or(io::ErrorKind::InvalidInput)?
        .canonicalize()?;
    if !target.starts_with(&runtime) || !parent.starts_with(&runtime) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let display = crate::storage::friendly_path(&target);
    let print: Vec<u16> = display.as_os_str().encode_wide().collect();
    let substitute: Vec<u16> = if print.starts_with(&[b'\\' as u16, b'\\' as u16]) {
        "\\??\\UNC\\"
            .encode_utf16()
            .chain(print[2..].iter().copied())
            .collect()
    } else {
        "\\??\\"
            .encode_utf16()
            .chain(print.iter().copied())
            .collect()
    };
    let path_bytes = (substitute.len() + print.len() + 2) * 2;
    if 16 + path_bytes > 16 * 1024 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // REPARSE_DATA_BUFFER with a MountPointReparseBuffer (8-byte fixed payload).
    let mut buffer = Vec::with_capacity(16 + path_bytes);
    buffer.extend(0xa000_0003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
    for field in [
        (8 + path_bytes) as u16,
        0, // Reserved
        0, // SubstituteNameOffset
        (substitute.len() * 2) as u16,
        ((substitute.len() + 1) * 2) as u16, // PrintNameOffset
        (print.len() * 2) as u16,
    ] {
        buffer.extend(field.to_le_bytes());
    }
    for unit in substitute.into_iter().chain([0]).chain(print).chain([0]) {
        buffer.extend(unit.to_le_bytes());
    }
    fs::create_dir(link)?;
    let result = (|| {
        let file = fs::OpenOptions::new()
            .access_mode(GENERIC_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(link)?;
        let mut returned = 0;
        // SAFETY: handle owns an empty directory opened as a reparse point; the
        // bounded input buffer has the documented mount-point layout and length.
        if unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                null_mut(),
                0,
                &mut returned,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir(link); // Never traverse or delete the target.
    }
    result
}
