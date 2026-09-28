use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// Default ceiling for any single blocking file/subprocess operation that
/// touches a path supplied by the caller (e.g. a scanned media file). Chosen
/// to comfortably exceed a slow spinning disk or network share while still
/// surfacing a stale/disconnected mount point (which otherwise blocks the
/// underlying syscall forever on macOS) within one command's run.
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(20);

/// Assumed floor throughput for a whole-file read, in MB/s, used to scale the
/// timeout to the size of the file.
///
/// Deliberately far under real hardware: a USB SSD measured 158 MB/s on the
/// library that produced this mechanism. It is a floor, not an estimate, so a
/// degraded link still finishes rather than being declared unreachable.
pub const MIN_READ_RATE_MB_S_DEFAULT: u64 = 20;

/// Ceiling for a `stat`, which is *not* proportional to file size, so unlike a
/// read it is correctly bounded by a constant.
///
/// This is the liveness check: on a stale or disconnected mount `fs::metadata`
/// is itself one of the calls that blocks forever, so a mount that answers it
/// promptly can be trusted to have reported a real size.
pub const STAT_TIMEOUT: Duration = Duration::from_secs(5);

static MIN_READ_RATE_OVERRIDE: OnceLock<u64> = OnceLock::new();

/// Overrides the assumed floor read rate, from config. Call once at startup,
/// before any scan begins; later calls are ignored, as with the qlmanage
/// concurrency override this mirrors.
pub fn set_min_read_rate_mb_s(rate: u64) {
    let _ = MIN_READ_RATE_OVERRIDE.set(rate);
}

/// Pure resolution of the effective rate, split out from the `OnceLock` so the
/// "override if present, else default" logic is unit-testable without touching
/// process-wide state. Same split as `heic::resolve_qlmanage_concurrency`.
fn resolve_min_read_rate(override_val: Option<u64>) -> u64 {
    match override_val {
        // A zero rate would mean an unbounded timeout, reintroducing the hang
        // the whole mechanism exists to prevent. The config layer rejects it;
        // this refuses it again rather than trusting a single gate.
        Some(0) | None => MIN_READ_RATE_MB_S_DEFAULT,
        Some(n) => n,
    }
}

pub fn min_read_rate_mb_s() -> u64 {
    resolve_min_read_rate(MIN_READ_RATE_OVERRIDE.get().copied())
}

/// How long a whole-file read of `size_bytes` may take before it is considered
/// stalled rather than merely large.
///
/// A constant ceiling cannot tell those apart. Measured 2026-08-12: a healthy
/// 3.7 GB video on a drive sustaining 158 MB/s needs ~23s, and was being
/// skipped by a fixed 20s cap with a message blaming the drive. File sizes do
/// not change, so such a file was skipped on every run, forever.
///
/// Never returns less than `DEFAULT_IO_TIMEOUT`, so small files behave exactly
/// as before. Total by construction: saturating arithmetic (a debug build
/// panics on overflow, and computing a timeout must never be the thing that
/// crashes a scan) and a zero rate falls back to the default rather than
/// dividing by zero or returning an unbounded timeout. The config layer
/// rejects a zero rate too; this stays total regardless of who calls it.
pub fn timeout_for_size(size_bytes: u64, rate_mb_s: u64) -> Duration {
    let rate = if rate_mb_s == 0 {
        MIN_READ_RATE_MB_S_DEFAULT
    } else {
        rate_mb_s
    };
    let bytes_per_sec = rate.saturating_mul(1_000_000);
    let secs = size_bytes / bytes_per_sec.max(1);
    Duration::from_secs(secs).max(DEFAULT_IO_TIMEOUT)
}

#[derive(Debug)]
pub enum IoRunError {
    Deadline(Duration),
    NoProgress(Duration),
    Capacity { active: usize, limit: usize },
    Spawn(std::io::Error),
    Disconnected,
}

impl IoRunError {
    pub fn into_io_error(self) -> std::io::Error {
        let kind = match self {
            Self::Deadline(_) | Self::NoProgress(_) => std::io::ErrorKind::TimedOut,
            Self::Capacity { .. } => std::io::ErrorKind::WouldBlock,
            Self::Spawn(_) | Self::Disconnected => std::io::ErrorKind::Other,
        };
        std::io::Error::new(kind, self)
    }
}

impl std::fmt::Display for IoRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deadline(d) => write!(f, "operation timed out after {}s", d.as_secs()),
            Self::NoProgress(d) => write!(f, "no read progress for {}s", d.as_secs()),
            Self::Capacity { active, limit } => {
                write!(f, "I/O worker limit reached ({active}/{limit} active)")
            }
            Self::Spawn(e) => write!(f, "could not spawn I/O worker: {e}"),
            Self::Disconnected => write!(f, "I/O worker disconnected"),
        }
    }
}

impl std::error::Error for IoRunError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoWorkerStats {
    pub active: usize,
    pub maximum: usize,
    pub timed_out_active: usize,
    pub capacity_refusals: u64,
}

struct PoolState {
    active: usize,
    maximum: usize,
    timed_out_active: usize,
    capacity_refusals: u64,
}

struct IoWorkerPool {
    state: Mutex<PoolState>,
    available: Condvar,
}

impl IoWorkerPool {
    fn new(maximum: usize) -> Self {
        Self {
            state: Mutex::new(PoolState {
                active: 0,
                maximum,
                timed_out_active: 0,
                capacity_refusals: 0,
            }),
            available: Condvar::new(),
        }
    }

    fn stats(&self) -> IoWorkerStats {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        IoWorkerStats {
            active: state.active,
            maximum: state.maximum,
            timed_out_active: state.timed_out_active,
            capacity_refusals: state.capacity_refusals,
        }
    }

    fn acquire(self: &Arc<Self>) -> Result<WorkerPermit, IoRunError> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.active >= state.maximum {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                state.capacity_refusals = state.capacity_refusals.saturating_add(1);
                let active = state.active;
                let limit = state.maximum;
                let error = IoRunError::Capacity { active, limit };
                let timed_out_active = state.timed_out_active;
                drop(state);
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        active,
                        limit,
                        timed_out_active,
                        "I/O worker pool is saturated; later files will be retried"
                    );
                }
                return Err(error);
            }
            let (next, _) = self
                .available
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner());
            state = next;
        }
        state.active += 1;
        Ok(WorkerPermit {
            pool: Arc::clone(self),
            lifecycle: Arc::new(Mutex::new(WorkerLifecycle::default())),
        })
    }
}

#[derive(Default)]
struct WorkerLifecycle {
    finished: bool,
    timed_out: bool,
}

struct WorkerPermit {
    pool: Arc<IoWorkerPool>,
    lifecycle: Arc<Mutex<WorkerLifecycle>>,
}

impl WorkerPermit {
    fn mark_timed_out(lifecycle: &Mutex<WorkerLifecycle>, pool: &IoWorkerPool) {
        let mut worker = lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        if !worker.finished && !worker.timed_out {
            worker.timed_out = true;
            let mut state = pool.state.lock().unwrap_or_else(|e| e.into_inner());
            state.timed_out_active += 1;
        }
    }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        let mut worker = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        worker.finished = true;
        let mut state = self.pool.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active -= 1;
        if worker.timed_out {
            state.timed_out_active -= 1;
        }
        self.pool.available.notify_one();
    }
}

fn default_max_io_workers() -> usize {
    let cores = thread::available_parallelism().map_or(4, |n| n.get());
    cores.saturating_mul(4).clamp(32, 128)
}

fn global_pool() -> &'static Arc<IoWorkerPool> {
    static POOL: OnceLock<Arc<IoWorkerPool>> = OnceLock::new();
    POOL.get_or_init(|| Arc::new(IoWorkerPool::new(default_max_io_workers())))
}

pub fn worker_stats() -> IoWorkerStats {
    global_pool().stats()
}

pub fn set_max_io_workers(maximum: usize) {
    assert!((1..=256).contains(&maximum));
    let pool = global_pool();
    let mut state = pool.state.lock().unwrap_or_else(|e| e.into_inner());
    state.maximum = maximum;
    pool.available.notify_all();
}

/// Runs `f` on a helper thread and waits up to `timeout` for it to finish.
/// Use this to bound any blocking call (`std::fs::*`, `image::open`, a
/// subprocess `.wait()`) that could otherwise block indefinitely against an
/// unresponsive mount point.
pub fn run_with_timeout<T, F>(timeout: Duration, f: F) -> Result<T, IoRunError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_with_timeout_in(global_pool(), timeout, f)
}

fn run_with_timeout_in<T, F>(
    pool: &Arc<IoWorkerPool>,
    timeout: Duration,
    f: F,
) -> Result<T, IoRunError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_with_timeout_in_using(pool, timeout, f, |job| {
        thread::Builder::new()
            .name("videre-io".into())
            .spawn(job)
            .map(|_| ())
    })
}

fn run_with_timeout_in_using<T, F, S>(
    pool: &Arc<IoWorkerPool>,
    timeout: Duration,
    f: F,
    spawn: S,
) -> Result<T, IoRunError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    S: FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
{
    let permit = pool.acquire()?;
    let lifecycle = Arc::clone(&permit.lifecycle);
    let (tx, rx) = mpsc::channel();
    if let Err(e) = spawn(Box::new(move || {
        let _permit = permit;
        let _ = tx.send(f());
    })) {
        return Err(IoRunError::Spawn(e));
    }
    match rx.recv_timeout(timeout) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            WorkerPermit::mark_timed_out(&lifecycle, pool);
            Err(IoRunError::Deadline(timeout))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(IoRunError::Disconnected),
    }
}

struct ProgressState {
    last_read: Instant,
    cancelled: bool,
}

/// Shares one read-progress clock across every reader in a hashing operation.
#[derive(Clone)]
pub struct ProgressHandle {
    state: Arc<Mutex<ProgressState>>,
}

impl ProgressHandle {
    pub fn wrap<R: Read + Seek>(&self, inner: R) -> ProgressReader<R> {
        ProgressReader {
            inner,
            progress: self.clone(),
        }
    }

    pub fn check_cancelled(&self) -> std::io::Result<()> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.cancelled {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "hash operation was cancelled after no read progress",
            ))
        } else {
            Ok(())
        }
    }

    fn read_progressed(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.last_read = Instant::now();
    }
}

/// A reader that reports actual bytes received, not elapsed time or file size.
pub struct ProgressReader<R> {
    inner: R,
    progress: ProgressHandle,
}

impl<R: Read + Seek> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.progress.check_cancelled()?;
        let count = self.inner.read(buf)?;
        if count > 0 {
            self.progress.read_progressed();
        }
        Ok(count)
    }
}

impl<R: Read + Seek> Seek for ProgressReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.progress.check_cancelled()?;
        self.inner.seek(pos)
    }
}

pub fn run_with_progress_timeout<T, F>(idle: Duration, f: F) -> Result<T, IoRunError>
where
    F: FnOnce(ProgressHandle) -> T + Send + 'static,
    T: Send + 'static,
{
    run_with_progress_timeout_in(global_pool(), idle, f)
}

fn run_with_progress_timeout_in<T, F>(
    pool: &Arc<IoWorkerPool>,
    idle: Duration,
    f: F,
) -> Result<T, IoRunError>
where
    F: FnOnce(ProgressHandle) -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = pool.acquire()?;
    let lifecycle = Arc::clone(&permit.lifecycle);
    let progress = ProgressHandle {
        state: Arc::new(Mutex::new(ProgressState {
            last_read: Instant::now(),
            cancelled: false,
        })),
    };
    let observer = progress.clone();
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("videre-io-progress".into())
        .spawn(move || {
            let _permit = permit;
            let _ = tx.send(f(progress));
        })
        .map_err(IoRunError::Spawn)?;

    loop {
        let left = {
            let state = observer.state.lock().unwrap_or_else(|e| e.into_inner());
            idle.saturating_sub(state.last_read.elapsed())
        };
        match rx.recv_timeout(left) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(IoRunError::Disconnected),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut state = observer.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.last_read.elapsed() < idle {
                    continue;
                }
                if let Ok(value) = rx.try_recv() {
                    return Ok(value);
                }
                state.cancelled = true;
                drop(state);
                WorkerPermit::mark_timed_out(&lifecycle, pool);
                return Err(IoRunError::NoProgress(idle));
            }
        }
    }
}

/// Runs `f` with a timeout scaled to the size of `path`.
///
/// For operations that read a file *whole*, where duration really is
/// proportional to size. Not for decoding: a QuickLook poster frame reads a
/// fraction of a video, so scaling by full size would hand a multi-GB file
/// minutes for work that should take a second, and QuickLook hanging on a
/// container with no video track is a known failure mode this project already
/// had to bound.
///
/// The `stat` is bounded separately and *first*, by a constant. That ordering
/// is the safety property: a dead mount fails there, in `STAT_TIMEOUT`, and the
/// read is never attempted, so a large file on a dead mount cannot hang for its
/// scaled timeout. A failed or timed-out `stat` is reported as `TimedOut`
/// rather than guessed around.
pub fn run_with_timeout_for_path<T, F>(path: &Path, f: F) -> Result<T, PathIoError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_with_timeout_for_path_detailed(path, f)
}

/// Which phase ran out of time, and how long it was given.
///
/// Exists so a caller can describe the failure without asking the filesystem
/// again. `hash_file` used to format its message by calling `std::fs::metadata`
/// **unbounded** on the very path that had just timed out - on a stale mount
/// that is the call that blocks forever, so the error handler hung in exactly
/// the scenario the timeout was protecting against.
#[derive(Debug)]
pub enum PathIoError {
    Stat(IoRunError),
    Read(IoRunError),
    StatUnavailable,
}

impl PathIoError {
    pub fn into_io_error(self) -> std::io::Error {
        match self {
            Self::Stat(error) | Self::Read(error) => error.into_io_error(),
            Self::StatUnavailable => {
                std::io::Error::new(std::io::ErrorKind::NotFound, "file metadata unavailable")
            }
        }
    }
    /// Phrasing that distinguishes "the drive did not answer" from "this was
    /// slow", which the single old message could not.
    pub fn describe(&self, path: &Path) -> String {
        match self {
            Self::Stat(IoRunError::Deadline(d)) => format!(
                "could not read {} after {}s (the drive did not respond - is it connected?)",
                path.display(),
                d.as_secs()
            ),
            Self::Read(IoRunError::Deadline(d)) => format!(
                "timed out reading {} after {}s (file may be unreachable - is its drive connected?)",
                path.display(),
                d.as_secs()
            ),
            Self::Stat(e) | Self::Read(e) => format!("could not read {}: {e}", path.display()),
            Self::StatUnavailable => format!("could not stat {}", path.display()),
        }
    }
}

/// `run_with_timeout_for_path`, reporting which phase timed out and after how
/// long, so the caller never has to touch the filesystem to explain itself.
pub fn run_with_timeout_for_path_detailed<T, F>(path: &Path, f: F) -> Result<T, PathIoError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let owned = path.to_path_buf();
    let size = run_with_timeout(STAT_TIMEOUT, move || {
        std::fs::metadata(&owned).map(|m| m.len()).ok()
    })
    .map_err(PathIoError::Stat)?
    .ok_or(PathIoError::StatUnavailable)?;
    let budget = timeout_for_size(size, min_read_rate_mb_s());
    run_with_timeout(budget, f).map_err(PathIoError::Read)
}

/// Outcome of waiting on a child process with a deadline.
#[derive(Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Success,
    Failed,
    TimedOut,
}

/// Polls `child` for completion, killing it if it hasn't exited within
/// `timeout`. Unlike a raw blocking `.wait()`/`.status()`, this guarantees
/// the caller gets control back within roughly `timeout` even if the child
/// itself is stuck on an unresponsive mount point.
pub fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> WaitOutcome {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    WaitOutcome::Success
                } else {
                    WaitOutcome::Failed
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return WaitOutcome::TimedOut;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return WaitOutcome::Failed,
        }
    }
}

/// Whether a missing file's absence can be trusted as a real deletion.
///
/// `false` when the parent directory is *also* missing: that means the
/// directory, or the whole volume, is gone rather than this one file having
/// been deleted. `videre prune` uses this to avoid deleting every row for an
/// unmounted drive, which additionally destroys the embeddings and cached
/// thumbnails for those hashes (hours of recompute, against minutes to
/// re-scan the rows themselves).
///
/// Deliberately not a mount-table lookup. On macOS `/Volumes` reports the same
/// filesystem as `/` when nothing is mounted there, so an unmounted volume
/// leaves nothing to query; telling "unmounted" from "deleted directory" apart
/// exactly needs either platform-specific enumeration or state recorded at
/// scan time. This rule needs neither and behaves identically on Linux.
///
/// Bounded by `run_with_timeout`, because a stale NFS or SMB mount can hang
/// `metadata` indefinitely and a safety check that hangs is not a safety
/// check. A timeout returns `false`: an unanswerable question must never
/// authorise a deletion.
///
/// A path with no parent (`/`, or a bare relative name) also returns `false`,
/// since there is nothing to corroborate the absence against.
pub fn absence_is_trustworthy(path: &Path) -> bool {
    absence_is_trustworthy_in(global_pool(), path)
}

fn absence_is_trustworthy_in(pool: &Arc<IoWorkerPool>, path: &Path) -> bool {
    absence_evidence_in(pool, path) == AbsenceEvidence::ParentPresent
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsenceEvidence {
    ParentPresent,
    ParentMissing,
    Unknown,
}

/// Distinguishes a confirmed missing parent from a stat that could not run.
/// Only the former may be overridden by `prune --prune-unreachable`.
pub fn absence_evidence(path: &Path) -> AbsenceEvidence {
    absence_evidence_in(global_pool(), path)
}

fn absence_evidence_in(pool: &Arc<IoWorkerPool>, path: &Path) -> AbsenceEvidence {
    let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return AbsenceEvidence::Unknown;
    };
    let parent = parent.to_path_buf();
    match run_with_timeout_in(pool, STAT_TIMEOUT, move || std::fs::metadata(parent)) {
        Ok(Ok(meta)) if meta.is_dir() => AbsenceEvidence::ParentPresent,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            AbsenceEvidence::ParentMissing
        }
        _ => AbsenceEvidence::Unknown,
    }
}

#[cfg(test)]
mod absence_tests {
    use super::*;

    #[test]
    fn saturated_parent_check_is_unknown_not_absent() {
        let pool = Arc::new(IoWorkerPool::new(1));
        let held = pool.acquire().unwrap();
        assert!(!absence_is_trustworthy_in(
            &pool,
            Path::new("/a/possibly-present.jpg")
        ));
        assert_eq!(
            absence_evidence_in(&pool, Path::new("/a/possibly-present.jpg")),
            AbsenceEvidence::Unknown
        );
        drop(held);
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("videre-absence-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_missing_file_in_an_existing_directory_is_trustworthy() {
        let dir = tmp("present");
        assert!(absence_is_trustworthy(&dir.join("gone.jpg")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_in_a_missing_directory_is_not() {
        // The unmounted-volume shape: neither the file nor its parent exists.
        let dir = tmp("absent");
        let nested = dir.join("subdir");
        assert!(!absence_is_trustworthy(&nested.join("gone.jpg")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_real_present_file_is_trustworthy_too() {
        // The function only judges the parent; callers ask it about paths they
        // already know are missing, but it must not depend on that.
        let dir = tmp("realfile");
        let f = dir.join("here.jpg");
        std::fs::write(&f, b"x").unwrap();
        assert!(absence_is_trustworthy(&f));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_without_a_usable_parent_is_not_trustworthy() {
        // Nothing to corroborate the absence against, so refuse rather than
        // authorise a deletion.
        assert!(!absence_is_trustworthy(Path::new("/")));
        assert!(!absence_is_trustworthy(Path::new("bare-name.jpg")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn returns_ok_when_operation_finishes_before_timeout() {
        let result = run_with_timeout(Duration::from_secs(1), || 42);
        assert!(result.is_ok());
        assert_eq!(result.ok(), Some(42));
    }

    #[test]
    fn returns_timed_out_when_operation_exceeds_timeout() {
        let result = run_with_timeout(Duration::from_millis(50), || {
            thread::sleep(Duration::from_secs(5));
            42
        });
        assert!(result.is_err());
    }

    #[test]
    fn pool_never_exceeds_limit_and_refuses_after_one_second() {
        let pool = Arc::new(IoWorkerPool::new(1));
        let (release_tx, release_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let first_pool = Arc::clone(&pool);
        let first = thread::spawn(move || {
            run_with_timeout_in(&first_pool, Duration::from_secs(5), move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let would_run = Arc::clone(&ran);
        let error = run_with_timeout_in(&pool, Duration::from_secs(1), move || {
            would_run.store(true, Ordering::SeqCst);
        })
        .unwrap_err();
        assert!(matches!(
            error,
            IoRunError::Capacity {
                active: 1,
                limit: 1
            }
        ));
        assert!(!ran.load(Ordering::SeqCst));
        assert_eq!(pool.stats().active, 1);
        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
        assert_eq!(pool.stats().active, 0);
    }

    #[test]
    fn timeout_keeps_permit_until_worker_exits() {
        let pool = Arc::new(IoWorkerPool::new(1));
        let (release_tx, release_rx) = mpsc::channel();
        let error = run_with_timeout_in(&pool, Duration::from_millis(20), move || {
            release_rx.recv().unwrap();
        })
        .unwrap_err();
        assert!(matches!(error, IoRunError::Deadline(_)));
        assert_eq!(pool.stats().active, 1);
        assert_eq!(pool.stats().timed_out_active, 1);
        release_tx.send(()).unwrap();
        for _ in 0..100 {
            if pool.stats().active == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(pool.stats().active, 0);
        assert_eq!(pool.stats().timed_out_active, 0);
    }

    #[test]
    fn completion_at_deadline_releases_permit_once() {
        let pool = Arc::new(IoWorkerPool::new(1));
        for _ in 0..20 {
            let (release_tx, release_rx) = mpsc::channel();
            let worker_pool = Arc::clone(&pool);
            let caller = thread::spawn(move || {
                run_with_timeout_in(&worker_pool, Duration::from_millis(5), move || {
                    let _ = release_rx.recv_timeout(Duration::from_millis(5));
                    7
                })
            });
            thread::sleep(Duration::from_millis(5));
            let _ = release_tx.send(());
            let result = caller.join().unwrap();
            assert!(matches!(result, Ok(7) | Err(IoRunError::Deadline(_))));
            for _ in 0..100 {
                if pool.stats().active == 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(pool.stats().active, 0);
            assert_eq!(pool.stats().timed_out_active, 0);
        }
    }

    #[test]
    fn panic_and_spawn_failure_release_reservations() {
        let pool = Arc::new(IoWorkerPool::new(1));
        let panic = run_with_timeout_in(&pool, Duration::from_secs(1), || -> u8 {
            panic!("test worker panic")
        });
        assert!(matches!(panic, Err(IoRunError::Disconnected)));
        let failed_spawn = run_with_timeout_in_using(
            &pool,
            Duration::from_secs(1),
            || 1u8,
            |_job| Err(std::io::Error::other("injected spawn failure")),
        );
        assert!(matches!(failed_spawn, Err(IoRunError::Spawn(_))));
        assert_eq!(pool.stats().active, 0);
        assert!(matches!(
            run_with_timeout_in(&pool, Duration::from_secs(1), || 2),
            Ok(2)
        ));
    }

    #[test]
    fn capacity_converts_to_would_block_not_timeout() {
        let error = IoRunError::Capacity {
            active: 2,
            limit: 2,
        }
        .into_io_error();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(!error.to_string().contains("drive did not respond"));
    }

    #[test]
    fn wait_with_timeout_returns_success_for_fast_process() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        assert_eq!(
            wait_with_timeout(&mut child, Duration::from_secs(5)),
            WaitOutcome::Success
        );
    }

    #[test]
    fn wait_with_timeout_kills_and_returns_timed_out_for_slow_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let start = Instant::now();
        assert_eq!(
            wait_with_timeout(&mut child, Duration::from_millis(200)),
            WaitOutcome::TimedOut
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[cfg(test)]
mod size_timeout_tests {
    use super::*;

    #[test]
    fn a_small_file_gets_exactly_the_old_constant() {
        // Nothing may get *less* time than before this existed.
        assert_eq!(timeout_for_size(0, 20), DEFAULT_IO_TIMEOUT);
        assert_eq!(timeout_for_size(1_000_000, 20), DEFAULT_IO_TIMEOUT);
        // The crossover: below 400 MB at 20 MB/s the default still wins.
        assert_eq!(timeout_for_size(399_000_000, 20), DEFAULT_IO_TIMEOUT);
    }

    #[test]
    fn the_file_that_produced_this_bug_now_gets_enough_time() {
        // 3.7 GB, skipped on a drive measured at 158 MB/s where a full read
        // needs ~23s, against a fixed 20s cap.
        let t = timeout_for_size(3_700_000_000, 20);
        assert_eq!(t.as_secs(), 185);
        assert!(t.as_secs() > 23, "must exceed the real read time");
    }

    #[test]
    fn the_largest_file_in_the_measured_library_is_bounded_and_finite() {
        assert_eq!(timeout_for_size(5_720_000_000, 20).as_secs(), 286);
    }

    #[test]
    fn a_zero_rate_falls_back_rather_than_dividing_by_zero() {
        // An unbounded timeout would reintroduce the hang this prevents.
        assert_eq!(timeout_for_size(3_700_000_000, 0).as_secs(), 185);
    }

    #[test]
    fn absurd_sizes_neither_panic_nor_overflow() {
        // A debug build panics on overflowing arithmetic, and computing a
        // timeout must never be the thing that crashes a scan.
        let t = timeout_for_size(u64::MAX, 1);
        assert!(t >= DEFAULT_IO_TIMEOUT);
        assert_eq!(timeout_for_size(u64::MAX, u64::MAX), DEFAULT_IO_TIMEOUT);
    }

    #[test]
    fn resolve_uses_the_override_but_refuses_zero() {
        assert_eq!(resolve_min_read_rate(Some(50)), 50);
        assert_eq!(resolve_min_read_rate(None), MIN_READ_RATE_MB_S_DEFAULT);
        assert_eq!(resolve_min_read_rate(Some(0)), MIN_READ_RATE_MB_S_DEFAULT);
    }

    #[test]
    fn a_dead_path_fails_at_the_stat_rather_than_running_the_body() {
        // The safety property: no size means no read, so a large file on a
        // dead mount cannot hang for its scaled timeout.
        let r = run_with_timeout_for_path(
            std::path::Path::new("/nonexistent/videre/definitely-not-here"),
            || 42,
        );
        assert!(r.is_err());
    }

    #[test]
    fn a_real_file_runs_the_body() {
        let d = std::env::temp_dir().join(format!("videre-sz-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("x.bin");
        std::fs::write(&f, b"hello").unwrap();
        assert_eq!(run_with_timeout_for_path(&f, || 42).ok(), Some(42));
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod timeout_reporting_tests {
    use super::*;

    #[test]
    fn an_unreachable_path_reports_the_stat_phase_not_a_read() {
        // A path that cannot be stat'd fails in the stat phase. The old code
        // reported every failure as "timed out reading ... after 20s", which
        // named the wrong phase and the wrong duration.
        let missing = Path::new("/nonexistent-videre-test/definitely/not/here");
        let r = run_with_timeout_for_path_detailed(missing, || 1u8);
        assert!(matches!(r.unwrap_err(), PathIoError::StatUnavailable));
    }

    #[test]
    fn describe_distinguishes_a_dead_drive_from_a_slow_file() {
        let p = Path::new("/some/file.mov");
        let stat = PathIoError::Stat(IoRunError::Deadline(STAT_TIMEOUT)).describe(p);
        let read = PathIoError::Read(IoRunError::Deadline(Duration::from_secs(185))).describe(p);
        assert!(stat.contains("did not respond"), "{stat}");
        assert!(read.contains("185s"), "{read}");
        assert_ne!(stat, read);
    }

    #[test]
    fn path_capacity_is_not_described_as_a_dead_drive() {
        let path = Path::new("/some/file.mov");
        let err = PathIoError::Stat(IoRunError::Capacity {
            active: 1,
            limit: 1,
        });
        assert_eq!(err.into_io_error().kind(), std::io::ErrorKind::WouldBlock);
        let err = PathIoError::Read(IoRunError::Capacity {
            active: 1,
            limit: 1,
        });
        let message = err.describe(path);
        assert!(message.contains("worker limit"), "{message}");
        assert!(!message.contains("drive did not respond"), "{message}");
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    use std::io::{Cursor, Read, Seek};

    #[test]
    fn one_byte_keeps_a_read_alive_past_old_total_budget() {
        for declared_size in [1_000_000_u64, 100_000_000_000_u64] {
            let pool = Arc::new(IoWorkerPool::new(1));
            // The stream (10 x 30 ms) outlasts the idle window twice over,
            // while each gap stays a fifth of it: margin enough for a loaded
            // CI runner's scheduling, which ate the earlier 15-of-40 ms.
            let result =
                run_with_progress_timeout_in(&pool, Duration::from_millis(150), move |h| {
                    let mut reader = h.wrap(Cursor::new([1_u8; 10]));
                    let mut byte = [0];
                    for _ in 0..10 {
                        reader.read_exact(&mut byte).unwrap();
                        thread::sleep(Duration::from_millis(30));
                    }
                    (declared_size, byte[0])
                });
            assert!(matches!(result, Ok((size, 1)) if size == declared_size));
        }
    }

    #[test]
    fn no_bytes_for_idle_window_times_out() {
        let pool = Arc::new(IoWorkerPool::new(1));
        let (release_tx, release_rx) = mpsc::channel();
        let result = run_with_progress_timeout_in(&pool, Duration::from_millis(20), move |_h| {
            release_rx.recv().unwrap();
        });
        assert!(matches!(result, Err(IoRunError::NoProgress(_))));
        assert_eq!(pool.stats().timed_out_active, 1);
        release_tx.send(()).unwrap();
    }

    #[test]
    fn zero_byte_eof_does_not_refresh_progress() {
        let pool = Arc::new(IoWorkerPool::new(1));
        let (release_tx, release_rx) = mpsc::channel();
        let result = run_with_progress_timeout_in(&pool, Duration::from_millis(20), move |h| {
            let mut reader = h.wrap(Cursor::new([]));
            assert_eq!(reader.read(&mut [0]).unwrap(), 0);
            release_rx.recv().unwrap();
        });
        assert!(matches!(result, Err(IoRunError::NoProgress(_))));
        release_tx.send(()).unwrap();
    }

    #[test]
    fn cancelled_reader_stops_before_next_read_or_seek() {
        let pool = Arc::new(IoWorkerPool::new(1));
        let (release_tx, release_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let result = run_with_progress_timeout_in(&pool, Duration::from_millis(20), move |h| {
            let mut reader = h.wrap(Cursor::new([1_u8; 2]));
            release_rx.recv().unwrap();
            let read = reader.read(&mut [0]).unwrap_err().kind();
            let seek = reader.seek(std::io::SeekFrom::Start(0)).unwrap_err().kind();
            result_tx.send((read, seek)).unwrap();
        });
        assert!(matches!(result, Err(IoRunError::NoProgress(_))));
        release_tx.send(()).unwrap();
        let (read, seek) = result_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(read, std::io::ErrorKind::TimedOut);
        assert_eq!(seek, std::io::ErrorKind::TimedOut);
    }

    #[test]
    fn completion_at_idle_deadline_has_one_result_and_releases_permit() {
        let pool = Arc::new(IoWorkerPool::new(1));
        for _ in 0..20 {
            let (release_tx, release_rx) = mpsc::channel();
            let caller_pool = Arc::clone(&pool);
            let caller = thread::spawn(move || {
                run_with_progress_timeout_in(&caller_pool, Duration::from_millis(5), move |_h| {
                    let _ = release_rx.recv_timeout(Duration::from_millis(5));
                    9
                })
            });
            thread::sleep(Duration::from_millis(5));
            let _ = release_tx.send(());
            let result = caller.join().unwrap();
            assert!(matches!(result, Ok(9) | Err(IoRunError::NoProgress(_))));
            for _ in 0..100 {
                if pool.stats().active == 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(pool.stats().active, 0);
            assert_eq!(pool.stats().timed_out_active, 0);
        }
    }
}
