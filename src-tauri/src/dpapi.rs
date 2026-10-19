//! Password protection with the Windows Data Protection API.
//!
//! Secrets are bound to the current Windows user account: they decrypt silently
//! for that user and are useless anywhere else.

use anyhow::{Context as _, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

const ENTROPY: &[u8] = b"rdp-app/connection-password/v1";

pub fn protect(secret: &str) -> anyhow::Result<String> {
    if secret.is_empty() {
        return Ok(String::new());
    }
    let sealed = imp::protect(secret.as_bytes()).context("encrypt password")?;
    Ok(STANDARD.encode(sealed))
}

pub fn unprotect(sealed: &str) -> anyhow::Result<String> {
    if sealed.is_empty() {
        return Ok(String::new());
    }
    let bytes = STANDARD.decode(sealed).context("stored password is corrupt")?;
    let plain = imp::unprotect(&bytes).context("decrypt password (was it saved by another Windows user?)")?;
    match String::from_utf8(plain) {
        Ok(s) => Ok(s),
        Err(_) => bail!("stored password is not valid text"),
    }
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr().cast_mut(),
        }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: the API allocated `cbData` bytes at `pbData` with LocalAlloc.
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe {
            let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
        }
        v
    }

    pub fn protect(data: &[u8]) -> windows::core::Result<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(super::ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &input,
                PCWSTR::null(),
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )?;
        }
        Ok(take(out))
    }

    pub fn unprotect(data: &[u8]) -> windows::core::Result<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(super::ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )?;
        }
        Ok(take(out))
    }
}

#[cfg(not(windows))]
mod imp {
    // Development fallback only; the app targets Windows.
    pub fn protect(data: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(data.to_vec())
    }
    pub fn unprotect(data: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip() {
        let sealed = super::protect("p@ss:w0rd:with:colons").unwrap();
        assert_ne!(sealed, "p@ss:w0rd:with:colons");
        assert_eq!(super::unprotect(&sealed).unwrap(), "p@ss:w0rd:with:colons");
        assert_eq!(super::protect("").unwrap(), "");
    }
}
