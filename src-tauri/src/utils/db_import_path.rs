use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum ManagedPathError {
    Invalid(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ManagedPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => formatter.write_str(reason),
            Self::Io(error) => write!(formatter, "Managed file is unavailable: {error}"),
        }
    }
}

impl From<std::io::Error> for ManagedPathError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Require a portable single filename, including when running on Unix with Windows input.
fn validate_filename(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || path.is_absolute()
        || path.file_name() != Some(std::ffi::OsStr::new(name))
    {
        return Err("Expected a single portable filename".to_string());
    }
    // Windows device names are special even with a file extension.
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
    {
        return Err("Reserved filename".to_string());
    }
    Ok(())
}

fn is_ordinary_file(metadata: &std::fs::Metadata) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            // FILE_ATTRIBUTE_REPARSE_POINT
            return false;
        }
    }
    true
}

fn resolve_file(dir: &Path, filename: &str, allow_new: bool) -> Result<PathBuf, ManagedPathError> {
    validate_filename(filename).map_err(ManagedPathError::Invalid)?;
    let directory = std::fs::canonicalize(dir)?;
    let candidate = directory.join(filename);
    match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) if is_ordinary_file(&metadata) => {
            let canonical = std::fs::canonicalize(&candidate)?;
            if canonical.parent() != Some(directory.as_path()) {
                return Err(ManagedPathError::Invalid(
                    "File must stay in the managed directory".to_string(),
                ));
            }
            Ok(canonical)
        }
        Err(error) if allow_new && error.kind() == std::io::ErrorKind::NotFound => Ok(candidate),
        Err(error) => Err(ManagedPathError::Io(error)),
        _ => Err(ManagedPathError::Invalid(
            "Expected an ordinary file in the managed directory".to_string(),
        )),
    }
}

/// Remote imports select only existing files in the administrator-owned directory.
/// The directory must be writable only by trusted local operators.
pub fn resolve_custom_db_import_path(dir: &Path, filename: &str) -> Result<PathBuf, String> {
    resolve_file(dir, filename, false).map_err(|error| error.to_string())
}

pub fn validate_account_id(account_id: &str) -> Result<(), String> {
    validate_filename(account_id)
}

pub fn resolve_account_file_path(dir: &Path, account_id: &str) -> Result<PathBuf, String> {
    resolve_account_file_path_checked(dir, account_id).map_err(|error| error.to_string())
}

/// Preserve filesystem error kinds for callers that must distinguish stale files from transient IO.
pub fn resolve_account_file_path_checked(
    dir: &Path,
    account_id: &str,
) -> Result<PathBuf, ManagedPathError> {
    validate_account_id(account_id).map_err(ManagedPathError::Invalid)?;
    resolve_file(dir, &format!("{account_id}.json"), false)
}

/// New accounts may create one ordinary file, after all path components are validated.
pub fn resolve_account_write_path(dir: &Path, account_id: &str) -> Result<PathBuf, String> {
    validate_account_id(account_id)?;
    resolve_file(dir, &format!("{account_id}.json"), true).map_err(|error| error.to_string())
}

pub fn read_managed_account_file(dir: &Path, account_id: &str) -> Result<String, String> {
    let path = resolve_account_file_path(dir, account_id)?;
    let raw = std::fs::read_to_string(path)
        .map_err(|_| "Managed account file is unavailable".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| "Managed account file is malformed".to_string())?;
    if value.get("id").and_then(serde_json::Value::as_str) != Some(account_id) {
        return Err("Account ID does not match its managed filename".to_string());
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_path_resolution_preserves_io_error_kinds() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            resolve_account_file_path_checked(root.path(), "missing"),
            Err(ManagedPathError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(matches!(
            resolve_account_file_path_checked(root.path(), "../outside"),
            Err(ManagedPathError::Invalid(_))
        ));
    }

    #[test]
    fn managed_paths_reject_traversal_absolute_device_and_stream_names() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("acc1.json"), br#"{"id":"acc1"}"#).unwrap();
        assert!(read_managed_account_file(root.path(), "acc1").is_ok());
        for name in [
            "",
            ".",
            "..",
            "../outside",
            "..\\outside",
            "/outside",
            "C:\\outside",
            "C:outside",
            "\\\\server\\share",
            "acc:stream",
            "NUL",
            "con.txt",
            "COM1",
            "trailing.",
            "trailing ",
        ] {
            assert!(
                resolve_account_write_path(root.path(), name).is_err(),
                "accepted {name:?}"
            );
        }
        assert!(resolve_custom_db_import_path(root.path(), "acc1.json").is_ok());
        assert!(resolve_custom_db_import_path(root.path(), "missing.db").is_err());
        assert!(resolve_account_write_path(root.path(), "new-account").is_ok());
    }

    #[test]
    fn managed_reads_reject_mismatched_identity_and_directories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("acc1.json"), br#"{"id":"other"}"#).unwrap();
        std::fs::create_dir(root.path().join("directory.json")).unwrap();
        assert!(read_managed_account_file(root.path(), "acc1").is_err());
        assert!(resolve_account_write_path(root.path(), "directory").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn managed_paths_reject_symlinks_and_dangling_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked.json")).unwrap();
        std::os::unix::fs::symlink(
            root.path().join("absent"),
            root.path().join("dangling.json"),
        )
        .unwrap();
        assert!(resolve_account_file_path(root.path(), "linked").is_err());
        assert!(resolve_account_write_path(root.path(), "dangling").is_err());
    }
}
