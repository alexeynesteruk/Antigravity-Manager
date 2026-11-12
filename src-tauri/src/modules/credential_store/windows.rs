use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::ptr;

#[repr(C)]
struct FileTime {
    low: u32,
    high: u32,
}
#[repr(C)]
struct Credential {
    flags: u32,
    kind: u32,
    target: *const u16,
    comment: *const u16,
    last_written: FileTime,
    blob_size: u32,
    blob: *const u8,
    persist: u32,
    attribute_count: u32,
    attributes: *const c_void,
    alias: *const u16,
    username: *const u16,
}

#[link(name = "advapi32")]
extern "system" {
    fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
    fn CredReadW(
        target: *const u16,
        kind: u32,
        flags: u32,
        credential: *mut *mut Credential,
    ) -> i32;
    fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
    fn CredFree(buffer: *mut c_void);
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}
fn target(key: &str) -> Result<Vec<u16>, String> {
    if key.contains('\0') {
        return Err("Invalid credential-store entry identity".into());
    }
    Ok(wide(&format!("Antigravity-Manager.ManagedSecrets/{key}")))
}

pub(super) fn set_secret(key: &str, secret: &[u8]) -> Result<(), String> {
    if secret.len() > 2000 {
        return Err("Credential-store chunk exceeds the native storage limit".into());
    }
    set_named_secret(
        &format!("Antigravity-Manager.ManagedSecrets/{key}"),
        "Antigravity-Manager",
        secret,
    )
}

pub(super) fn set_named_secret(target: &str, username: &str, secret: &[u8]) -> Result<(), String> {
    if target.contains('\0') || username.contains('\0') || secret.len() > 65536 {
        return Err("Invalid native credential-store payload".into());
    }
    let target = wide(target);
    let username = wide(username);
    let credential = Credential {
        flags: 0,
        kind: 1,
        target: target.as_ptr(),
        comment: ptr::null(),
        last_written: FileTime { low: 0, high: 0 },
        blob_size: secret.len() as u32,
        blob: secret.as_ptr(),
        persist: 2,
        attribute_count: 0,
        attributes: ptr::null(),
        alias: ptr::null(),
        username: username.as_ptr(),
    };
    // The data is copied synchronously by WinCred; all pointers remain live.
    if unsafe { CredWriteW(&credential, 0) } == 0 {
        return Err(format!(
            "Windows credential-store write failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

pub(super) fn get_secret(key: &str) -> Result<Vec<u8>, String> {
    let target = format!("Antigravity-Manager.ManagedSecrets/{key}");
    let value = read_named_secret(&target)?;
    if value.len() > 2000 {
        return Err("Windows credential-store returned an oversized chunk".into());
    }
    Ok(value)
}

/// Shared native reader also used by the pre-existing CLI credential adapter.
pub(crate) fn read_named_secret(target: &str) -> Result<Vec<u8>, String> {
    if target.contains('\0') {
        return Err("Invalid credential-store identity".into());
    }
    let target = wide(target);
    let mut credential = ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), 1, 0, &mut credential) } == 0 {
        return Err(format!(
            "Windows credential-store read failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    struct ReadGuard(*mut Credential);
    impl Drop for ReadGuard {
        fn drop(&mut self) {
            unsafe {
                CredFree(self.0.cast());
            }
        }
    }
    if credential.is_null() {
        return Err("Windows credential-store returned no entry".into());
    }
    let _guard = ReadGuard(credential);
    let credential = unsafe { &*credential };
    if credential.blob_size == 0 || credential.blob_size > 65536 || credential.blob.is_null() {
        return Err("Windows credential-store returned an invalid entry".into());
    }
    Ok(
        unsafe { std::slice::from_raw_parts(credential.blob, credential.blob_size as usize) }
            .to_vec(),
    )
}

pub(super) fn delete_secret(key: &str) -> Result<(), String> {
    let target = target(key)?;
    if unsafe { CredDeleteW(target.as_ptr(), 1, 0) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(1168) {
            return Err(format!("Windows credential-store deletion failed: {error}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "explicit native-store smoke check; never touches an existing entry"]
    fn isolated_native_credential_store_roundtrip() {
        let key = format!("v1/smoke/{}/0", uuid::Uuid::new_v4());
        super::set_secret(&key, b"synthetic-vault-check").unwrap();
        let result = super::get_secret(&key);
        let cleanup = super::delete_secret(&key);
        assert_eq!(result.unwrap(), b"synthetic-vault-check");
        cleanup.unwrap();
        assert!(super::get_secret(&key).is_err());
    }
}
