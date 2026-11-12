//! Platform credential storage. Secret bytes never appear in process arguments,
//! error messages, temporary files, or an unprotected fallback store.

const SERVICE: &str = "Antigravity-Manager.ManagedSecrets";
const MAX_SECRET_BYTES: usize = 2000;
const MAX_NAMED_SECRET_BYTES: usize = 64 * 1024;

fn validate_named_identity(service: &str, account: &str) -> Result<(), String> {
    if [service, account].iter().any(|value| {
        value.is_empty()
            || value.len() > 512
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/-_.".contains(&byte))
    }) {
        return Err("Invalid credential-store service or account identifier".into());
    }
    Ok(())
}

fn validate_named_secret(secret: &[u8]) -> Result<(), String> {
    if secret.is_empty()
        || secret.len() > MAX_NAMED_SECRET_BYTES
        || std::str::from_utf8(secret).is_err()
    {
        return Err("Invalid native application credential payload".into());
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<(), String> {
    if !key.starts_with("v1/")
        || key.len() > 512
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/-_.".contains(&byte))
    {
        return Err("Invalid managed credential identifier".into());
    }
    Ok(())
}

fn validate_secret(secret: &[u8]) -> Result<(), String> {
    if secret.is_empty()
        || secret.len() > MAX_SECRET_BYTES
        || !secret
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(byte))
    {
        return Err("Invalid managed credential chunk".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use security_framework::passwords;

    pub(super) fn set_secret(key: &str, secret: &[u8]) -> Result<(), String> {
        validate_key(key)?;
        validate_secret(secret)?;
        // Native Keychain access: no `security -w` argv exposure or `-A` ACL.
        passwords::set_generic_password(SERVICE, key, secret)
            .map_err(|_| "macOS Keychain could not save the managed credential".into())
    }

    pub(super) fn get_secret(key: &str) -> Result<Vec<u8>, String> {
        validate_key(key)?;
        let secret = passwords::get_generic_password(SERVICE, key)
            .map_err(|_| "macOS Keychain credential is missing, locked, or unavailable")?;
        validate_secret(&secret)?;
        Ok(secret)
    }

    pub(super) fn delete_secret(key: &str) -> Result<(), String> {
        validate_key(key)?;
        match passwords::delete_generic_password(SERVICE, key) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == -25300 => Ok(()), // errSecItemNotFound
            Err(_) => Err("macOS Keychain could not remove the managed credential".into()),
        }
    }

    pub(super) fn set_named_secret(
        service: &str,
        account: &str,
        secret: &[u8],
    ) -> Result<(), String> {
        validate_named_identity(service, account)?;
        validate_named_secret(secret)?;
        passwords::set_generic_password(service, account, secret)
            .map_err(|_| "macOS Keychain could not save the application credential".into())
    }

    pub(super) fn get_named_secret(service: &str, account: &str) -> Result<Vec<u8>, String> {
        validate_named_identity(service, account)?;
        let secret = passwords::get_generic_password(service, account).map_err(|_| {
            "macOS Keychain application credential is missing, locked, or unavailable"
        })?;
        validate_named_secret(&secret)?;
        Ok(secret)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::{Duration, Instant};

    const TOOL_TIMEOUT: Duration = Duration::from_secs(10);
    const POLL_INTERVAL: Duration = Duration::from_millis(10);

    fn tool(operation: &str, key: &str) -> Command {
        named_tool(operation, SERVICE, key, false)
    }

    fn named_tool(operation: &str, service: &str, account: &str, external: bool) -> Command {
        let mut command = Command::new("secret-tool");
        command.arg(operation);
        if operation == "store" {
            command.arg(if external {
                "--label=Antigravity application credential"
            } else {
                "--label=Antigravity Manager managed credential"
            });
            if external {
                command.arg("--collection=/org/freedesktop/secrets/collection/login");
            }
        }
        // Existing AGY uses the go-keyring `username` attribute. Keep manager
        // vault entries on their separate `account` attribute contract.
        command.args([
            "service",
            service,
            if external { "username" } else { "account" },
            account,
        ]);
        command
    }

    fn nonblocking(fd: RawFd) -> Result<(), String> {
        // Only descriptors for this newly spawned child's pipes are changed.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err("Could not configure credential-store communication".into());
        }
        Ok(())
    }

    struct ManagedChild {
        child: Child,
        reaped: bool,
        completed: bool,
    }

    impl Drop for ManagedChild {
        fn drop(&mut self) {
            if self.completed {
                return;
            }
            // process_group(0) gives this helper its own group. Also terminate a
            // stuck descendant holding a pipe, without touching the app's group.
            unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            if !self.reaped {
                let deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < deadline {
                    match self.child.try_wait() {
                        Ok(Some(_)) => break,
                        Err(_) => break,
                        Ok(None) => std::thread::sleep(POLL_INTERVAL),
                    }
                }
            }
        }
    }

    fn run_tool(
        mut command: Command,
        input: Option<&[u8]>,
        capture: bool,
        timeout: Duration,
        max_bytes: usize,
    ) -> Result<(ExitStatus, Vec<u8>), String> {
        if input.is_some_and(|input| input.len() > max_bytes) {
            return Err("Credential-store input exceeds its size limit".into());
        }
        command
            .process_group(0)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(if capture {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null());
        let deadline = Instant::now() + timeout;
        let mut managed = ManagedChild {
            child: command.spawn().map_err(|_| {
                "Linux Secret Service is unavailable; install secret-tool and unlock a session keyring"
            })?,
            reaped: false,
            completed: false,
        };
        let mut stdin = managed.child.stdin.take();
        let mut stdout = managed.child.stdout.take();
        if let Some(pipe) = &stdin {
            nonblocking(pipe.as_raw_fd())?;
        }
        if let Some(pipe) = &stdout {
            nonblocking(pipe.as_raw_fd())?;
        }
        let input = input.unwrap_or_default();
        let mut written = 0;
        let mut output = Vec::new();
        let mut status = None;
        loop {
            if Instant::now() >= deadline {
                return Err("Linux Secret Service operation timed out".into());
            }
            if let Some(pipe) = &mut stdin {
                match pipe.write(&input[written..]) {
                    Ok(0) => return Err("Linux Secret Service stopped accepting input".into()),
                    Ok(bytes) => written += bytes,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        return Err("Linux Secret Service could not accept the credential".into())
                    }
                }
                if written == input.len() {
                    // EOF is required: secret-tool stores all piped bytes verbatim.
                    stdin = None;
                }
            }
            if let Some(pipe) = &mut stdout {
                let mut buffer = [0u8; 512];
                match pipe.read(&mut buffer) {
                    Ok(0) => stdout = None,
                    Ok(bytes) => {
                        output.extend_from_slice(&buffer[..bytes]);
                        if output.len() > max_bytes + 1 {
                            return Err(
                                "Linux Secret Service returned an oversized credential".into()
                            );
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        return Err("Linux Secret Service credential could not be read".into())
                    }
                }
            }
            if status.is_none() {
                status = managed
                    .child
                    .try_wait()
                    .map_err(|_| "Linux Secret Service process status is unavailable")?;
                managed.reaped = status.is_some();
            }
            if let Some(status) = status {
                if stdout.is_none() {
                    managed.completed = true;
                    return Ok((status, output));
                }
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    pub(super) fn set_secret(key: &str, secret: &[u8]) -> Result<(), String> {
        validate_key(key)?;
        validate_secret(secret)?;
        let (status, _) = run_tool(
            tool("store", key),
            Some(secret),
            false,
            TOOL_TIMEOUT,
            MAX_SECRET_BYTES,
        )?;
        if status.success() {
            Ok(())
        } else {
            Err("Linux Secret Service could not save the managed credential".into())
        }
    }

    pub(super) fn get_secret(key: &str) -> Result<Vec<u8>, String> {
        validate_key(key)?;
        let output = lookup(tool("lookup", key), MAX_SECRET_BYTES)?;
        validate_secret(&output)?;
        Ok(output)
    }

    fn lookup(command: Command, max_bytes: usize) -> Result<Vec<u8>, String> {
        let (status, mut output) = run_tool(command, None, true, TOOL_TIMEOUT, max_bytes)?;
        if !status.success() {
            return Err(
                "Linux Secret Service credential is missing, locked, or unavailable".into(),
            );
        }
        // secret-tool lookup adds one LF; never trim arbitrary credential bytes.
        if output.last() == Some(&b'\n') {
            output.pop();
        }
        Ok(output)
    }

    pub(super) fn delete_secret(key: &str) -> Result<(), String> {
        validate_key(key)?;
        let (status, _) = run_tool(
            tool("clear", key),
            None,
            false,
            TOOL_TIMEOUT,
            MAX_SECRET_BYTES,
        )?;
        if status.success() {
            Ok(())
        } else {
            // Do not misclassify a locked/unavailable keyring as an absent item.
            Err("Linux Secret Service could not remove the managed credential".into())
        }
    }

    pub(super) fn set_named_secret(
        service: &str,
        account: &str,
        secret: &[u8],
    ) -> Result<(), String> {
        validate_named_identity(service, account)?;
        validate_named_secret(secret)?;
        let (status, _) = run_tool(
            named_tool("store", service, account, true),
            Some(secret),
            false,
            TOOL_TIMEOUT,
            MAX_NAMED_SECRET_BYTES,
        )?;
        if status.success() {
            Ok(())
        } else {
            Err("Linux Secret Service could not save the application credential".into())
        }
    }

    pub(super) fn get_named_secret(service: &str, account: &str) -> Result<Vec<u8>, String> {
        validate_named_identity(service, account)?;
        let secret = lookup(
            named_tool("lookup", service, account, true),
            MAX_NAMED_SECRET_BYTES,
        )?;
        validate_named_secret(&secret)?;
        Ok(secret)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn secret_tool_arguments_contain_identifiers_only() {
            let command = tool("store", "v1/account/test/0");
            let args: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy())
                .collect();
            assert_eq!(args[0], "store");
            assert!(args.iter().any(|arg| arg == SERVICE));
            assert_eq!(args.last().unwrap(), "v1/account/test/0");
            assert!(!args.iter().any(|arg| arg == "QUJD"));
            assert!(args.iter().any(|arg| arg == "account"));
            let external = named_tool("store", "gemini", "antigravity", true);
            let args: Vec<_> = external
                .get_args()
                .map(|arg| arg.to_string_lossy())
                .collect();
            assert!(args
                .iter()
                .any(|arg| arg == "--collection=/org/freedesktop/secrets/collection/login"));
            assert!(args.iter().any(|arg| arg == "username"));
            assert!(!args.iter().any(|arg| arg == "account"));
            let lookup = named_tool("lookup", "gemini", "antigravity", true);
            assert!(lookup.get_args().any(|arg| arg == "username"));
        }

        #[test]
        fn pipe_transfer_uses_stdin_and_preserves_lookup_bytes() {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "cat; printf '\\n'"]);
            let (status, output) = run_tool(
                command,
                Some(b"QUJD"),
                true,
                Duration::from_secs(2),
                MAX_SECRET_BYTES,
            )
            .unwrap();
            assert!(status.success());
            assert_eq!(output, b"QUJD\n");
        }

        #[test]
        fn stalled_helpers_and_oversized_output_fail_without_secret_errors() {
            let mut stalled = Command::new("/bin/sh");
            stalled.args(["-c", "sleep 5"]);
            let started = Instant::now();
            let error = run_tool(
                stalled,
                None,
                true,
                Duration::from_millis(50),
                MAX_SECRET_BYTES,
            )
            .unwrap_err();
            assert!(error.contains("timed out"));
            assert!(started.elapsed() < Duration::from_secs(2));
            let mut oversized = Command::new("/bin/sh");
            oversized.args(["-c", "head -c 4096 /dev/zero"]);
            assert!(run_tool(
                oversized,
                None,
                true,
                Duration::from_secs(2),
                MAX_SECRET_BYTES
            )
            .unwrap_err()
            .contains("oversized"));
        }

        #[test]
        fn named_payloads_use_bounded_stdin_beyond_the_managed_chunk_limit() {
            let payload = vec![b'a'; MAX_NAMED_SECRET_BYTES];
            assert!(validate_secret(&payload).is_err());
            validate_named_secret(&payload).unwrap();
            assert!(validate_named_secret(&vec![b'a'; MAX_NAMED_SECRET_BYTES + 1]).is_err());
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "cat; printf '\\n'"]);
            let (status, output) = run_tool(
                command,
                Some(&payload),
                true,
                Duration::from_secs(5),
                MAX_NAMED_SECRET_BYTES,
            )
            .unwrap();
            assert!(status.success());
            assert_eq!(output.len(), MAX_NAMED_SECRET_BYTES + 1);
            assert_eq!(&output[..MAX_NAMED_SECRET_BYTES], payload);
            assert_eq!(output.last(), Some(&b'\n'));
        }
    }
}

pub(super) fn set_secret(key: &str, secret: &[u8]) -> Result<(), String> {
    platform::set_secret(key, secret)
}

pub(super) fn get_secret(key: &str) -> Result<Vec<u8>, String> {
    platform::get_secret(key)
}

pub(super) fn delete_secret(key: &str) -> Result<(), String> {
    platform::delete_secret(key)
}

pub(super) fn set_named_secret(service: &str, account: &str, secret: &[u8]) -> Result<(), String> {
    platform::set_named_secret(service, account, secret)
}

pub(super) fn get_named_secret(service: &str, account: &str) -> Result<Vec<u8>, String> {
    platform::get_named_secret(service, account)
}
