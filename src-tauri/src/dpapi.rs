//! Password protection, bound to the signed-in user.
//!
//! - **Windows**: the Data Protection API. Secrets decrypt silently for the current Windows
//!   account and are useless anywhere else.
//! - **macOS**: AES-256-GCM under a random key kept in the user's login Keychain (made on first
//!   use). The library file holds only ciphertext; without that Keychain item it can't be read.
//!
//! Either way, a sealed secret is base64 text in the library.

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
    let plain = imp::unprotect(&bytes).context("decrypt password (was it saved by another user, or on another PC?)")?;
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

/// AES-256-GCM sealing under a 32-byte key: `[version, nonce (12), ciphertext + tag]`. The
/// macOS backend's format; plain code, so it's tested on every platform.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod aead {
    use anyhow::{Context as _, bail};
    use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
    use ring::rand::{SecureRandom, SystemRandom};

    const VERSION: u8 = 1;

    pub fn key(bytes: &[u8]) -> anyhow::Result<LessSafeKey> {
        let key = UnboundKey::new(&AES_256_GCM, bytes).ok().context("bad key")?;
        Ok(LessSafeKey::new(key))
    }

    pub fn seal(key: &LessSafeKey, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new().fill(&mut nonce).ok().context("no randomness")?;
        let mut sealed = data.to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(super::ENTROPY),
            &mut sealed,
        )
        .ok()
        .context("encrypt")?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + sealed.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    pub fn open(key: &LessSafeKey, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        if data.first() != Some(&VERSION) || data.len() < 1 + NONCE_LEN {
            bail!("not a sealed secret");
        }
        let nonce = Nonce::try_assume_unique_for_key(&data[1..1 + NONCE_LEN])
            .ok()
            .context("nonce")?;
        let mut buf = data[1 + NONCE_LEN..].to_vec();
        let plain = key
            .open_in_place(nonce, Aad::from(super::ENTROPY), &mut buf)
            .ok()
            .context("wrong key, or the secret was altered")?;
        Ok(plain.to_vec())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::OnceLock;

    use anyhow::{Context as _, bail};
    use ring::aead::LessSafeKey;
    use ring::rand::{SecureRandom, SystemRandom};
    use security_framework::passwords::{get_generic_password, set_generic_password};

    /// The Keychain item holding the key.
    const SERVICE: &str = "com.jeromejaja.rdpapp";
    const ACCOUNT: &str = "library-key";
    /// errSecItemNotFound: there's no key yet.
    const NOT_FOUND: i32 = -25300;

    /// The key, read from (or first made in) the login Keychain, once per run.
    fn key() -> anyhow::Result<&'static LessSafeKey> {
        static KEY: OnceLock<LessSafeKey> = OnceLock::new();
        if let Some(key) = KEY.get() {
            return Ok(key);
        }
        let bytes = match get_generic_password(SERVICE, ACCOUNT) {
            Ok(bytes) if bytes.len() == 32 => bytes,
            Ok(_) => bail!("the Keychain item {SERVICE}/{ACCOUNT} isn't a key"),
            Err(e) if e.code() == NOT_FOUND => {
                let mut fresh = vec![0u8; 32];
                SystemRandom::new().fill(&mut fresh).ok().context("no randomness")?;
                set_generic_password(SERVICE, ACCOUNT, &fresh).context("save the key to the Keychain")?;
                fresh
            }
            // Any other failure (access denied, Keychain locked): never make a new key, or every
            // password sealed with the existing one would be lost.
            Err(e) => return Err(e).context("read the key from the Keychain"),
        };
        Ok(KEY.get_or_init(|| super::aead::key(&bytes).expect("a 32-byte key")))
    }

    pub fn protect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        super::aead::seal(key()?, data)
    }

    pub fn unprotect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        super::aead::open(key()?, data)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    // Development fallback only: other platforms aren't supported yet.
    pub fn protect(data: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(data.to_vec())
    }
    pub fn unprotect(data: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    // The platform round trip (the macOS one uses the real Keychain, so it isn't run in tests).
    #[cfg(windows)]
    #[test]
    fn round_trip() {
        let sealed = super::protect("p@ss:w0rd:with:colons").unwrap();
        assert_ne!(sealed, "p@ss:w0rd:with:colons");
        assert_eq!(super::unprotect(&sealed).unwrap(), "p@ss:w0rd:with:colons");
        assert_eq!(super::protect("").unwrap(), "");
    }

    #[test]
    fn aead_seals_and_opens_and_refuses_tampering_or_the_wrong_key() {
        let key = super::aead::key(&[7u8; 32]).unwrap();
        let sealed = super::aead::seal(&key, b"p@ss").unwrap();
        assert_ne!(&sealed[13..], b"p@ss");
        assert_eq!(super::aead::open(&key, &sealed).unwrap(), b"p@ss");
        // Each seal uses a fresh nonce.
        assert_ne!(super::aead::seal(&key, b"p@ss").unwrap(), sealed);
        let mut altered = sealed.clone();
        *altered.last_mut().unwrap() ^= 1;
        assert!(super::aead::open(&key, &altered).is_err());
        let other = super::aead::key(&[8u8; 32]).unwrap();
        assert!(super::aead::open(&other, &sealed).is_err());
        assert!(super::aead::open(&key, b"plain").is_err());
    }
}
