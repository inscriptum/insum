//! Lock / dirty-lease sidecar protocol (spec §9.2) — the CLI side.
//!
//! Two hidden sidecar files next to the target protect a source file from
//! concurrent writers and from overwriting unsaved editor state:
//!
//! - `.<name>.insum-lock` — a short-lived transactional lock around one
//!   commit. Created atomically (create-new), held for milliseconds,
//!   released on success and on error.
//! - `.<name>.insum-lease` — the editor's dirty lease. Held while the open
//!   document has unsaved changes; the CLI never creates one, only reads
//!   it (and recovers a stale one).
//!
//! Both files are JSON with the holder's pid, hostname, an operation id and
//! the creation time. Recovery: a holder provably dead on the same host is
//! broken immediately; a cross-host or unknown-host holder cannot be
//! probed and is only declared gone after `stale_after`; a LIVE same-host
//! holder is never broken, whatever its age. A corrupt file blocks
//! conservatively while fresh and is broken once it ages past
//! `stale_after` (it may be mid-write).
//!
//! The protocol document for the plugin side lives in the knowledge base
//! (`~/insum/DEVELOPMENT/insum/lock-protocol.um`); this module implements
//! the CLI half of it. No flock/fcntl advisory locks are used: a
//! create-new sidecar works identically on macOS, Linux and Windows and is
//! observable by the plugin with plain file APIs.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, UmError};

/// Suffixes of the two sidecar files.
pub const LOCK_SUFFIX: &str = ".insum-lock";
pub const LEASE_SUFFIX: &str = ".insum-lease";

/// Format version of the sidecar JSON.
const SIDECAR_FORMAT: u32 = 1;

/// Defaults tuned for a commit lock: a commit holds it for milliseconds;
/// a dead writer is detected immediately (same host) or within a minute.
impl LockConfig {
    pub fn defaults() -> Self {
        LockConfig {
            timeout: Duration::from_millis(5_000),
            poll_interval: Duration::from_millis(50),
            stale_after: Duration::from_secs(60),
        }
    }
}

/// Timing knobs of the acquire loop.
#[derive(Debug, Clone, Copy)]
pub struct LockConfig {
    /// How long `acquire` waits (polling) before reporting WRITE_LOCKED.
    pub timeout: Duration,
    /// Sleep between create-new attempts.
    pub poll_interval: Duration,
    /// Age after which a holder that cannot be probed (cross-host or
    /// unknown host) and corrupt files are declared gone. A live same-host
    /// holder is never broken, whatever its age.
    pub stale_after: Duration,
}

/// The recorded holder of a sidecar file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Holder {
    /// `"cli"` or `"plugin"`.
    pub kind: String,
    pub pid: u32,
    /// Best-effort host identification; only the equal-host pid check is
    /// trusted for liveness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
}

/// The JSON body of both sidecar files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    pub format: u32,
    /// `"lock"` or `"lease"`.
    pub kind: String,
    pub holder: Holder,
    /// Random operation id; release only removes the file when it still
    /// records this id.
    pub op_id: String,
    /// Epoch milliseconds at creation.
    pub created_at: u64,
    /// Free-form purpose (`"apply"`, `"migrate-ids"`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
}

impl Sidecar {
    fn new(kind: &str, holder_kind: &str, purpose: Option<&str>) -> Self {
        Sidecar {
            format: SIDECAR_FORMAT,
            kind: kind.to_owned(),
            holder: Holder {
                kind: holder_kind.to_owned(),
                pid: std::process::id(),
                hostname: current_hostname(),
            },
            op_id: new_op_id(),
            created_at: epoch_ms(),
            purpose: purpose.map(str::to_owned),
        }
    }

    fn write_atomic(&self, path: &Path) -> Result<(), UmError> {
        let body = serde_json::to_vec(self)
            .map_err(|err| UmError::internal(format!("sidecar does not serialize: {err}")))?;
        // create_new is the atomic step: exactly one writer wins
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::AlreadyExists {
                    UmError::new(ErrorCode::WriteLocked, "sidecar already exists")
                } else {
                    UmError::from(err)
                }
            })?;
        file.write_all(&body)
            .and_then(|_| file.flush())
            .map_err(UmError::from)?;
        Ok(())
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn new_op_id() -> String {
    // 12 hex chars from wall-clock nanoseconds mixed with the pid: not
    // cryptographic entropy, but collisions between concurrent holders on
    // one host are practically impossible, which is all op_id needs
    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{:08x}{:04x}", nanos, pid & 0xffff)
}

fn current_hostname() -> Option<String> {
    #[cfg(unix)]
    {
        // gethostname(2): works without a shell environment (agent runs
        // have no exported HOSTNAME)
        let mut buffer = [0u8; 256];
        let result =
            unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len()) };
        if result != 0 {
            return None;
        }
        let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        let name = String::from_utf8_lossy(&buffer[..end]).trim().to_owned();
        if name.is_empty() { None } else { Some(name) }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{
            ComputerNameDnsHostname, GetComputerNameExW,
        };
        let mut size: u32 = 0;
        unsafe {
            // first call: required buffer size in UTF-16 units
            let _ = GetComputerNameExW(ComputerNameDnsHostname, std::ptr::null_mut(), &mut size);
            if size == 0 {
                return None;
            }
            let mut buffer = vec![0u16; size as usize];
            if GetComputerNameExW(ComputerNameDnsHostname, buffer.as_mut_ptr(), &mut size) == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buffer[..size as usize]))
        }
    }
}

/// Is `pid` alive on this machine? Only meaningful when the recorded
/// hostname matches (or is unknown).
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0): ESRCH = no such process, EPERM = exists (other user)
        let result = unsafe { libc::kill(pid as i32, 0) };
        if result == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut exit_code: u32 = 0;
            let alive =
                GetExitCodeProcess(handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE;
            CloseHandle(handle);
            alive
        }
    }
}

fn sidecar_path(target: &Path, suffix: &str) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    target.with_file_name(format!(".{name}{suffix}"))
}

/// A sidecar larger than this is not a legitimate lock/lease — treat it as
/// corrupt without loading it into memory.
const SIDECAR_MAX_BYTES: u64 = 64 * 1024;

/// Read and parse a sidecar. `Ok(None)` covers both a missing and an
/// unparsable file; a foreign `format` parses but is rejected by callers
/// via `is_supported_sidecar`.
fn read_sidecar(path: &Path) -> Result<Option<Sidecar>, UmError> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(UmError::from(err)),
    };
    if metadata.len() > SIDECAR_MAX_BYTES {
        return Ok(None); // oversized: corrupt by definition
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(UmError::from(err)),
    };
    match serde_json::from_slice::<Sidecar>(&bytes) {
        Ok(sidecar) => Ok(Some(sidecar)),
        Err(_) => Ok(None), // corrupt: callers apply the age policy
    }
}

/// A sidecar of an unknown format version is foreign: it is never
/// interpreted, only aged (recovery rule of the protocol document).
fn is_supported_sidecar(sidecar: &Sidecar) -> bool {
    sidecar.format == SIDECAR_FORMAT && (sidecar.kind == "lock" || sidecar.kind == "lease")
}

/// Age of a sidecar file: the recorded creation time clamped by the file
/// mtime (a skewed clock may claim the future or the far past; the mtime is
/// written by the same machine that reads it and cannot lie as much).
fn sidecar_age(path: &Path, sidecar: Option<&Sidecar>) -> Duration {
    let mtime_ms = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    let recorded = sidecar.map(|s| s.created_at);
    let created = match (recorded, mtime_ms) {
        (Some(recorded), Some(mtime)) => recorded.max(mtime),
        (Some(recorded), None) => recorded,
        (None, Some(mtime)) => mtime,
        (None, None) => 0,
    };
    let now = epoch_ms();
    Duration::from_millis(now.saturating_sub(created))
}

/// Recovery rule of the protocol document: on the SAME host the pid check
/// is authoritative — a live holder is never broken, no matter how old
/// (breaking it would risk parallel commits and lost editor state). A
/// CROSS-HOST or unknown-host holder cannot be probed, so only age
/// (`stale_after`) declares it gone.
fn holder_is_gone(sidecar: &Sidecar, path: &Path, stale_after: Duration) -> bool {
    let same_host = match (&sidecar.holder.hostname, current_hostname()) {
        (Some(recorded), Some(current)) => recorded.as_str() == current.as_str(),
        // unknown host on either side: the pid check is not trustworthy
        _ => false,
    };
    if same_host {
        return !pid_alive(sidecar.holder.pid);
    }
    sidecar_age(path, Some(sidecar)) > stale_after
}

/// The state of the editor's dirty lease for a target file.
#[derive(Debug)]
pub enum LeaseState {
    /// No lease file.
    Free,
    /// A lease is held; `holder_pid` is informational.
    Held { holder: Holder, op_id: String },
    /// A lease exists but its holder is provably gone (dead pid on this
    /// host) or it is older than `stale_after`; `op_id` is `None` for a
    /// corrupt/foreign file.
    Stale { op_id: Option<String> },
    /// A fresh corrupt or foreign-format lease: it may be mid-write, so it
    /// blocks conservatively and is only broken after `stale_after`.
    Corrupt,
}

/// Inspect the dirty lease of `target` without touching it (§9.2: apply
/// refuses to write while a live lease exists). A corrupt or foreign
/// lease blocks conservatively while fresh (it may be mid-write) and
/// counts as stale once it ages past `stale_after`.
pub fn check_dirty_lease(target: &Path, stale_after: Duration) -> Result<LeaseState, UmError> {
    let path = sidecar_path(target, LEASE_SUFFIX);
    match read_sidecar(&path)? {
        None => {
            if path.exists() {
                // corrupt or unreadable: age decides
                if sidecar_age(&path, None) > stale_after {
                    Ok(LeaseState::Stale { op_id: None })
                } else {
                    Ok(LeaseState::Corrupt)
                }
            } else {
                Ok(LeaseState::Free)
            }
        }
        Some(sidecar) if !is_supported_sidecar(&sidecar) => {
            if sidecar_age(&path, Some(&sidecar)) > stale_after {
                Ok(LeaseState::Stale { op_id: None })
            } else {
                Ok(LeaseState::Corrupt)
            }
        }
        Some(sidecar) => {
            if holder_is_gone(&sidecar, &path, stale_after) {
                Ok(LeaseState::Stale {
                    op_id: Some(sidecar.op_id),
                })
            } else {
                Ok(LeaseState::Held {
                    holder: sidecar.holder,
                    op_id: sidecar.op_id,
                })
            }
        }
    }
}

/// Remove a stale (or aged-corrupt) lease; returns whether a file was
/// removed. A live lease is never touched.
pub fn break_stale_lease(target: &Path, stale_after: Duration) -> Result<bool, UmError> {
    let path = sidecar_path(target, LEASE_SUFFIX);
    match read_sidecar(&path)? {
        None => {
            // corrupt file: broken only when aged
            if path.exists() && sidecar_age(&path, None) > stale_after {
                std::fs::remove_file(&path).map_err(UmError::from)?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        Some(sidecar) if !is_supported_sidecar(&sidecar) => {
            if sidecar_age(&path, Some(&sidecar)) > stale_after {
                std::fs::remove_file(&path).map_err(UmError::from)?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        Some(sidecar) => {
            if holder_is_gone(&sidecar, &path, stale_after) {
                std::fs::remove_file(&path).map_err(UmError::from)?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

/// A held transactional commit lock. The protocol requires an explicit
/// `release` in both success and error paths; dropping without it leaves
/// the file in place (a known limitation — recovery is the stale
/// detection, and a retry in the same process waits out its own lock).
pub struct FileLock {
    path: PathBuf,
    op_id: String,
    released: bool,
}

impl std::fmt::Debug for FileLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileLock")
            .field("path", &self.path)
            .field("op_id", &self.op_id)
            .field("released", &self.released)
            .finish()
    }
}

impl FileLock {
    /// Atomically create `.<name>.insum-lock` next to `target`, waiting up
    /// to `config.timeout` and breaking provably-dead holders. `Err`
    /// `WRITE_LOCKED` when a live holder (or a fresh unparsable file)
    /// survives the wait.
    pub fn acquire(target: &Path, purpose: &str, config: &LockConfig) -> Result<FileLock, UmError> {
        let path = sidecar_path(target, LOCK_SUFFIX);
        let deadline = Instant::now() + config.timeout;
        loop {
            let sidecar = Sidecar::new("lock", "cli", Some(purpose));
            match sidecar.write_atomic(&path) {
                Ok(()) => {
                    return Ok(FileLock {
                        path,
                        op_id: sidecar.op_id,
                        released: false,
                    });
                }
                Err(err) if err.code == ErrorCode::WriteLocked => {
                    Self::wait_or_recover(&path, config, deadline)?;
                    if Instant::now() >= deadline {
                        return Err(Self::locked_error(target, &path)?);
                    }
                    std::thread::sleep(config.poll_interval);
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Decide whether the existing sidecar can be broken (dead holder,
    /// stale age, aged corrupt file) or the caller must keep waiting.
    fn wait_or_recover(path: &Path, config: &LockConfig, deadline: Instant) -> Result<(), UmError> {
        let sidecar = read_sidecar(path)?;
        let Some(sidecar) = sidecar else {
            // unparsable or vanished: only age proves it is not mid-write
            if sidecar_age(path, None) > config.stale_after {
                let _ = std::fs::remove_file(path);
            }
            return Ok(());
        };
        if !is_supported_sidecar(&sidecar) {
            // a foreign sidecar format: age-based recovery only
            if sidecar_age(path, Some(&sidecar)) > config.stale_after {
                let _ = std::fs::remove_file(path);
            }
            return Ok(());
        }
        if holder_is_gone(&sidecar, path, config.stale_after) && Instant::now() < deadline {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    fn locked_error(target: &Path, path: &Path) -> Result<UmError, UmError> {
        let sidecar = read_sidecar(path)?;
        let details = serde_json::json!({
            "lock_path": path.display().to_string(),
            "holder": sidecar.as_ref().map(|s| serde_json::json!({
                "kind": s.holder.kind,
                "pid": s.holder.pid,
            })),
        });
        let _ = target;
        Ok(UmError::new(
            ErrorCode::WriteLocked,
            "the file is held by another writer (commit lock)".to_owned(),
        )
        .with_hint(
            "Wait for the other writer to finish; a lock whose process died is broken automatically on the next attempt — unless the dead pid was reused by another process, then remove the lock sidecar manually.",
        )
        .with_details(details))
    }

    /// The recorded op id of this lock.
    pub fn op_id(&self) -> &str {
        &self.op_id
    }

    /// Remove the sidecar — but only if it still records this op id (a
    /// broken-and-reacquired lock must not be deleted by the dead holder).
    pub fn release(mut self) -> Result<(), UmError> {
        self.released = true;
        let sidecar = read_sidecar(&self.path)?;
        match sidecar {
            Some(recorded) if recorded.op_id == self.op_id => {
                std::fs::remove_file(&self.path).map_err(UmError::from)
            }
            _ => Ok(()), // gone or replaced: nothing to release
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if !self.released {
            // Explicit release is the protocol; a plain drop (error path
            // forgetting to release) leaves the file for stale recovery
            // rather than pretending it was released.
            // Nothing to do at runtime: keep the file.
        }
    }
}

/// The full write-readiness check used by the future apply/migrate-ids:
/// no live dirty lease and an acquirable commit lock.
pub fn acquire_write_lock(
    target: &Path,
    purpose: &str,
    config: &LockConfig,
) -> Result<FileLock, UmError> {
    loop {
        match check_dirty_lease(target, config.stale_after)? {
            LeaseState::Free => break,
            LeaseState::Stale { .. } => {
                // the stale file may have been replaced by a fresh live
                // lease between the check and the break — the break
                // refuses to touch a live one; re-check instead of writing
                if !break_stale_lease(target, config.stale_after)? {
                    continue;
                }
            }
            LeaseState::Corrupt => {
                return Err(UmError::new(
                    ErrorCode::WriteLocked,
                    "the dirty lease file of this file is unreadable and fresh",
                )
                .with_hint(
                    "A lease that is mid-write or damaged blocks the write conservatively; it recovers by age, or remove the lease sidecar manually if you are sure no editor is running.",
                ));
            }
            LeaseState::Held { holder, op_id } => {
                return Err(UmError::new(
                    ErrorCode::WriteLocked,
                    format!(
                        "the editor holds a dirty lease for this file (unsaved changes, holder pid {})",
                        holder.pid
                    ),
                )
                .with_hint(
                    "Save or close the document in the editor, then retry; the lease is released on save/close.",
                )
                .with_details(serde_json::json!({ "lease_op_id": op_id })));
            }
        }
    }
    FileLock::acquire(target, purpose, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_target(dir: &Path) -> PathBuf {
        let target = dir.join("page.um");
        std::fs::write(&target, b"placeholder").unwrap();
        target
    }

    fn fast_config() -> LockConfig {
        LockConfig {
            timeout: Duration::from_millis(300),
            poll_interval: Duration::from_millis(10),
            stale_after: Duration::from_secs(60),
        }
    }

    fn dead_pid() -> u32 {
        // a child that already exited: its pid is provably not alive
        #[cfg(unix)]
        let mut child = std::process::Command::new("true").spawn().unwrap();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit"])
            .spawn()
            .unwrap();
        let pid = child.id();
        let _ = child.wait();
        pid
    }

    fn write_sidecar(path: &Path, sidecar: &Sidecar) {
        std::fs::write(path, serde_json::to_vec(sidecar).unwrap()).unwrap();
    }

    /// Write a sidecar and age BOTH the recorded time and the file mtime
    /// (the age rule clamps created_at with the mtime, so both must lie in
    /// the past to make the file look old).
    fn write_aged_sidecar(path: &Path, mut sidecar: Sidecar) {
        sidecar.created_at = epoch_ms() - 120_000;
        write_sidecar(path, &sidecar);
        let aged = SystemTime::now() - Duration::from_secs(120);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(aged).unwrap();
    }

    #[test]
    fn acquire_and_release_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let lock = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        let lock_path = sidecar_path(&target, LOCK_SUFFIX);
        assert!(lock_path.exists(), "the sidecar exists while held");
        let recorded = read_sidecar(&lock_path).unwrap().unwrap();
        assert_eq!(recorded.kind, "lock");
        assert_eq!(recorded.holder.kind, "cli");
        assert_eq!(recorded.purpose.as_deref(), Some("apply"));
        assert_eq!(recorded.op_id, lock.op_id());
        lock.release().unwrap();
        assert!(!lock_path.exists(), "release removes the sidecar");
    }

    #[test]
    fn second_acquire_times_out_with_write_locked() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let _guard = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        let err = FileLock::acquire(&target, "other", &fast_config()).unwrap_err();
        assert_eq!(err.code, ErrorCode::WriteLocked);
        assert_eq!(err.code.exit_code(), 7);
        assert!(
            err.details.is_some(),
            "the error carries lock details for machines"
        );
    }

    #[test]
    fn lock_from_a_dead_process_is_broken() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let path = sidecar_path(&target, LOCK_SUFFIX);
        let mut sidecar = Sidecar::new("lock", "cli", Some("crashed"));
        sidecar.holder.pid = dead_pid();
        write_sidecar(&path, &sidecar);

        // the dead holder is broken immediately: acquire succeeds without
        // waiting for the timeout
        let started = Instant::now();
        let lock = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        assert!(
            started.elapsed() < fast_config().timeout,
            "recovery is immediate, not age-based"
        );
        // the new lock replaced the file with a fresh op id
        let recorded = read_sidecar(&path).unwrap().unwrap();
        assert_ne!(recorded.op_id, sidecar.op_id);
        lock.release().unwrap();
    }

    #[test]
    fn corrupt_lock_recovers_only_after_stale_age() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let path = sidecar_path(&target, LOCK_SUFFIX);
        std::fs::write(&path, b"{not json").unwrap();

        // fresh corrupt file: could be mid-write — WRITE_LOCKED
        let err = FileLock::acquire(&target, "apply", &fast_config()).unwrap_err();
        assert_eq!(err.code, ErrorCode::WriteLocked);
        assert!(path.exists(), "a fresh corrupt file is not removed");

        // aged corrupt file: provably abandoned — broken
        let aged = SystemTime::now() - Duration::from_secs(120);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(aged).unwrap();
        drop(file);
        let lock = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        lock.release().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn release_never_removes_a_reacquired_lock() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let path = sidecar_path(&target, LOCK_SUFFIX);
        let first = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        // the first holder "dies"; someone else breaks and reacquires
        let mut stale = read_sidecar(&path).unwrap().unwrap();
        stale.holder.pid = dead_pid();
        write_sidecar(&path, &stale);
        let second = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        // the dead first holder finally calls release: it must NOT delete
        // the second holder's lock
        first.release().unwrap();
        assert!(path.exists(), "the reacquired lock survives");
        second.release().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn atomic_race_lets_exactly_one_winner_through() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let barrier = barrier.clone();
            let winners = winners.clone();
            let target = target.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                if FileLock::acquire(&target, "race", &fast_config()).is_ok() {
                    winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                // others get WRITE_LOCKED (300 ms cap) — no crashes
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        // exactly one thread holds the lock at the end (the winner never
        // released); the losers all failed with WRITE_LOCKED
        assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(sidecar_path(&target, LOCK_SUFFIX).exists());
    }

    #[test]
    fn live_same_host_holder_survives_any_age() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let path = sidecar_path(&target, LEASE_SUFFIX);
        // our own pid: provably alive on this host, aged far beyond the cap
        let mut lease = Sidecar::new("lease", "plugin", Some("dirty"));
        lease.holder.pid = std::process::id();
        write_aged_sidecar(&path, lease);

        // a live editor lease older than stale_after is STILL HELD —
        // breaking it would write over unsaved editor state (§9.2 MUST)
        match check_dirty_lease(&target, fast_config().stale_after).unwrap() {
            LeaseState::Held { .. } => {}
            other => panic!("live lease must stay Held, got {other:?}"),
        }
        let err = acquire_write_lock(&target, "apply", &fast_config()).unwrap_err();
        assert_eq!(err.code, ErrorCode::WriteLocked);
        assert!(
            path.exists(),
            "a live holder is never broken, whatever its age"
        );
    }

    #[test]
    fn cross_host_holder_is_judged_by_age_only() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let path = sidecar_path(&target, LOCK_SUFFIX);
        // a live pid on a DIFFERENT host cannot be probed: age decides
        let mut lock = Sidecar::new("lock", "cli", Some("sync"));
        lock.holder.hostname = Some("other-machine.example".to_owned());
        lock.holder.pid = std::process::id();
        write_aged_sidecar(&path, lock);

        // aged cross-host lock: broken despite the (irrelevant) live pid
        let lock_guard = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        lock_guard.release().unwrap();
        assert!(!path.exists());

        // fresh cross-host lock: waits out the timeout (WRITE_LOCKED)
        let mut fresh = Sidecar::new("lock", "cli", Some("sync"));
        fresh.holder.hostname = Some("other-machine.example".to_owned());
        fresh.holder.pid = std::process::id();
        write_sidecar(&path, &fresh);
        let err = FileLock::acquire(&target, "apply", &fast_config()).unwrap_err();
        assert_eq!(err.code, ErrorCode::WriteLocked);
        assert!(path.exists());
    }

    #[test]
    fn foreign_format_is_never_interpreted_only_aged() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let path = sidecar_path(&target, LOCK_SUFFIX);
        let mut future = Sidecar::new("lock", "cli", Some("from-the-future"));
        future.format = 99;
        write_sidecar(&path, &future);
        // fresh foreign file: blocked, untouched
        let err = FileLock::acquire(&target, "apply", &fast_config()).unwrap_err();
        assert_eq!(err.code, ErrorCode::WriteLocked);
        assert!(path.exists());
        // aged foreign file: removed without interpretation
        write_aged_sidecar(&path, future);
        let lock_guard = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        lock_guard.release().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn unknown_hostname_is_judged_by_age_only() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let path = sidecar_path(&target, LOCK_SUFFIX);
        // no hostname recorded: the pid check is not trustworthy even on
        // the same machine — age is the only evidence
        let mut lock = Sidecar::new("lock", "cli", Some("from-unknown"));
        lock.holder.hostname = None;
        lock.holder.pid = std::process::id(); // alive, but unprovable
        write_aged_sidecar(&path, lock);
        let lock_guard = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        lock_guard.release().unwrap();
        assert!(!path.exists(), "aged unknown-host lock is broken by age");
    }

    #[test]
    fn corrupt_lease_blocks_fresh_and_recovers_aged() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let path = sidecar_path(&target, LEASE_SUFFIX);
        std::fs::write(&path, b"{half-written").unwrap();

        // fresh corrupt lease: blocks conservatively (may be mid-write)
        match check_dirty_lease(&target, fast_config().stale_after).unwrap() {
            LeaseState::Corrupt => {}
            other => panic!("expected Corrupt, got {other:?}"),
        }
        let err = acquire_write_lock(&target, "apply", &fast_config()).unwrap_err();
        assert_eq!(err.code, ErrorCode::WriteLocked);
        assert!(path.exists());

        // aged corrupt lease: stale, broken by the recovery path
        let aged = SystemTime::now() - Duration::from_secs(120);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(aged).unwrap();
        drop(file);
        match check_dirty_lease(&target, fast_config().stale_after).unwrap() {
            LeaseState::Stale { op_id: None } => {}
            other => panic!("expected Stale(None), got {other:?}"),
        }
        assert!(break_stale_lease(&target, fast_config().stale_after).unwrap());
        let lock_guard = acquire_write_lock(&target, "apply", &fast_config()).unwrap();
        lock_guard.release().unwrap();
    }

    #[test]
    fn oversized_sidecar_is_corrupt_without_loading() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let path = sidecar_path(&target, LEASE_SUFFIX);
        std::fs::write(&path, vec![b'x'; 128 * 1024]).unwrap();
        match check_dirty_lease(&target, fast_config().stale_after).unwrap() {
            LeaseState::Corrupt => {}
            other => panic!("oversized file must be corrupt, got {other:?}"),
        }
    }

    #[test]
    fn dirty_lease_blocks_write_readiness() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let lease_path = sidecar_path(&target, LEASE_SUFFIX);
        let mut lease = Sidecar::new("lease", "plugin", Some("dirty"));
        write_sidecar(&lease_path, &lease);

        match check_dirty_lease(&target, fast_config().stale_after).unwrap() {
            LeaseState::Held { holder, .. } => assert_eq!(holder.kind, "plugin"),
            other => panic!("expected Held, got {other:?}"),
        }
        let err = acquire_write_lock(&target, "apply", &fast_config()).unwrap_err();
        assert_eq!(err.code, ErrorCode::WriteLocked);
        assert!(err.message.contains("dirty lease"), "{}", err.message);
        assert!(lease_path.exists(), "a live lease is never touched");

        // after the holder dies the lease is stale and recoverable
        lease.holder.pid = dead_pid();
        write_sidecar(&lease_path, &lease);
        match check_dirty_lease(&target, fast_config().stale_after).unwrap() {
            LeaseState::Stale { .. } => {}
            other => panic!("expected Stale, got {other:?}"),
        }
        assert!(break_stale_lease(&target, fast_config().stale_after).unwrap());
        let lock = acquire_write_lock(&target, "apply", &fast_config()).unwrap();
        lock.release().unwrap();
    }

    #[test]
    fn sidecar_names_start_with_a_dot_and_hide_from_notes() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let lock = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        let name = sidecar_path(&target, LOCK_SUFFIX)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with('.'), "hidden per §9.2/§9.3: {name}");
        assert!(name.contains("page.um"), "target-bound: {name}");
        lock.release().unwrap();
    }

    #[test]
    fn sidecar_body_round_trips_through_json() {
        let sidecar = Sidecar::new("lock", "cli", Some("apply"));
        let bytes = serde_json::to_vec(&sidecar).unwrap();
        let parsed: Sidecar = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.format, SIDECAR_FORMAT);
        assert_eq!(parsed.kind, "lock");
        assert_eq!(parsed.holder.pid, std::process::id());
        assert_eq!(parsed.purpose.as_deref(), Some("apply"));
        assert!(!parsed.op_id.is_empty());
    }

    #[test]
    fn missing_target_directory_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("missing-dir").join("page.um");
        let err = FileLock::acquire(&target, "apply", &fast_config()).unwrap_err();
        assert_eq!(err.code, ErrorCode::IoError);
    }

    #[test]
    fn sidecar_json_is_human_inspectable() {
        let dir = tempfile::tempdir().unwrap();
        let target = temp_target(dir.path());
        let lock = FileLock::acquire(&target, "apply", &fast_config()).unwrap();
        let mut body = String::new();
        std::fs::File::open(sidecar_path(&target, LOCK_SUFFIX))
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert!(body.contains("\"format\":1"), "{body}");
        lock.release().unwrap();
    }
}
