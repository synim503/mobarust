//! Хранилище секретов (SEC-04).
//!
//! Windows: пароль шифруется DPAPI (`CryptProtectData`) — расшифровать его может
//! только та же учётная запись Windows на той же машине. В файле лежит только
//! зашифрованный блоб в hex. Открытым текстом пароли не хранятся никогда.
//! Другие ОС: сохранение паролей недоступно (функции возвращают None/false).

use std::collections::BTreeMap;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn path() -> std::path::PathBuf {
    crate::config::data_dir().join("secrets.json")
}

fn read_map() -> BTreeMap<String, String> {
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_map(m: &BTreeMap<String, String>) {
    if let Ok(json) = serde_json::to_vec_pretty(m) {
        let _ = crate::config::write_atomic(&path(), &json);
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Можно ли сохранять пароли на этой платформе.
pub fn available() -> bool {
    cfg!(windows)
}

pub fn get(key: &str) -> Option<String> {
    let _g = LOCK.lock().ok()?;
    let blob = unhex(read_map().get(key)?)?;
    let plain = unprotect(&blob)?;
    String::from_utf8(plain).ok()
}

pub fn set(key: &str, secret: &str) -> bool {
    let Some(blob) = protect(secret.as_bytes()) else { return false };
    let Ok(_g) = LOCK.lock() else { return false };
    let mut m = read_map();
    m.insert(key.to_string(), hex(&blob));
    write_map(&m);
    true
}

pub fn remove(key: &str) {
    if let Ok(_g) = LOCK.lock() {
        let mut m = read_map();
        if m.remove(key).is_some() {
            write_map(&m);
        }
    }
}

pub fn has(key: &str) -> bool {
    LOCK.lock().map(|_g| read_map().contains_key(key)).unwrap_or(false)
}

#[cfg(windows)]
fn protect(data: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData};
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        );
        if ok == 0 || out.pbData.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Some(v)
    }
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData};
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        );
        if ok == 0 || out.pbData.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Some(v)
    }
}

#[cfg(not(windows))]
fn protect(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(windows))]
fn unprotect(_data: &[u8]) -> Option<Vec<u8>> {
    None
}
