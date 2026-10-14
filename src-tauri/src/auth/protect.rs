use super::{AuthError, Result};
use zeroize::Zeroizing;

#[cfg(windows)]
fn transform(input: &[u8], decrypt: bool) -> Result<Zeroizing<Vec<u8>>> {
    use std::{ptr, slice};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    use zeroize::Zeroize;

    let length = u32::try_from(input.len()).map_err(|_| AuthError::Protection)?;
    let mut source = CRYPT_INTEGER_BLOB {
        cbData: length,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // DPAPI uses the current Windows user. No machine-wide flag or interactive prompt.
    let succeeded = unsafe {
        if decrypt {
            CryptUnprotectData(
                &mut source,
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptProtectData(
                &mut source,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if succeeded == 0 || output.pbData.is_null() {
        return Err(AuthError::Protection);
    }
    // The allocation is owned by DPAPI and released with LocalFree after copying.
    let result = unsafe {
        let bytes = slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let copy = Zeroizing::new(bytes.to_vec());
        bytes.zeroize();
        LocalFree(output.pbData.cast());
        copy
    };
    Ok(result)
}

pub(super) fn encrypt(input: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(windows)]
    {
        transform(input, false)
    }
    #[cfg(not(windows))]
    {
        let _ = input;
        Err(AuthError::Protection)
    }
}

pub(super) fn decrypt(input: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(windows)]
    {
        transform(input, true)
    }
    #[cfg(not(windows))]
    {
        let _ = input;
        Err(AuthError::Protection)
    }
}
