//! Track only processes we create; never enumerate other processes or their command lines.
use std::{
    ffi::{OsStr, OsString},
    io,
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
            LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
            STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

pub(super) struct Process {
    // Keep the original process handle and PID, even if a launcher exits early.
    // OwnedHandle closes handles only; the interactive Job has no kill-on-close.
    _process: OwnedHandle,
    _pid: u32,
    job: OwnedHandle,
}

impl Process {
    pub(super) fn start(executable: &Path, args: &[OsString]) -> io::Result<Self> {
        Self::spawn(executable, args, false)
    }

    fn spawn(executable: &Path, args: &[OsString], reader: bool) -> io::Result<Self> {
        let mut application: Vec<_> = executable.as_os_str().encode_wide().collect();
        if application.contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        application.push(0);
        let mut command_line = command_line(executable.as_os_str(), args)?;
        // Unnamed, non-inheritable Job. Interactive jobs have no kill-on-close;
        // reader jobs own their whole disposable process tree. Neither permits breakaway.
        let job = unsafe { CreateJobObjectW(null(), null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a fresh, exclusively owned handle.
        let job = unsafe { OwnedHandle::from_raw_handle(job) };
        if reader {
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: the Job and the correctly sized input structure are live.
            if unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of_val(&limits) as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let jobs = [job.as_raw_handle()];
        let mut attributes = Attributes::for_jobs(&jobs)?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = attributes.as_mut_ptr();
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: UTF-16 buffers are terminated and writable as required. The Job,
        // job list and aligned attribute storage remain alive throughout creation.
        // Assign at creation time so no early descendants escape an assignment race.
        // No handles (including credentials or the Job itself) are inherited.
        let ok = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
                null(),
                null(),
                &startup.StartupInfo,
                &mut info,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful creation transfers ownership of both fresh handles.
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        drop(thread);
        Ok(Self {
            _process: process,
            _pid: info.dwProcessId,
            job,
        })
    }

    pub(super) fn running(&self) -> io::Result<bool> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the live Job handle and exact output buffer size match this class.
        let ok = unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of_val(&info) as u32,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // Check our original handle as well. A PID is never used to reopen a
        // process, and an inconclusive API result must not permit a cookie read.
        match unsafe { WaitForSingleObject(self._process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(info.ActiveProcesses != 0),
            WAIT_TIMEOUT => Ok(true),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

// Termination is exposed only for the headless reader, never for Process/Browser.
pub(super) struct Reader(Process);
impl Reader {
    pub(super) fn start(executable: &Path, args: &[OsString]) -> io::Result<Self> {
        Process::spawn(executable, args, true).map(Self)
    }

    pub(super) fn running(&self) -> io::Result<bool> {
        self.0.running()
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        // SAFETY: this Job contains only this reader and its descendants.
        if unsafe { TerminateJobObject(self.0.job.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

// The attribute list is opaque but requires pointer-aligned storage. Only an
// initialized list is constructed, so Drop is valid on every subsequent error path.
struct Attributes(Vec<usize>);
impl Attributes {
    fn for_jobs(jobs: &[HANDLE]) -> io::Result<Self> {
        let mut bytes = 0;
        // The size-query call normally fails with ERROR_INSUFFICIENT_BUFFER.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = storage.as_mut_ptr().cast();
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut attributes = Self(storage);
        // SAFETY: the initialized list has one slot; the caller retains jobs until
        // after this attribute list is destroyed, including on failure.
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                jobs.as_ptr().cast(),
                size_of_val(jobs),
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(attributes)
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: storage holds a successfully initialized list and is still alive.
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

// Windows argv quoting: double backslashes before quotes and the closing quote.
// There is no shell, expansion or interpretation of argument text.
fn command_line(executable: &OsStr, args: &[OsString]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    for arg in std::iter::once(executable).chain(args.iter().map(OsString::as_os_str)) {
        if !line.is_empty() {
            line.push(b' ' as u16);
        }
        line.push(b'"' as u16);
        let mut slashes = 0;
        for unit in arg.encode_wide() {
            if unit == 0 {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            if unit == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            if unit == b'"' as u16 {
                line.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
            } else {
                line.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            }
            slashes = 0;
            line.push(unit);
        }
        line.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        line.push(b'"' as u16);
    }
    line.push(0);
    Ok(line)
}

#[cfg(test)]
#[path = "browser_job_tests.rs"]
mod tests;
