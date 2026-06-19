//! Single-instance guard for the desktop app.
//!
//! The data directory (`dirs::data_dir()/linewise-desktop`) is build-independent,
//! so a debug build and a downloaded release share the same `config.toml`,
//! `linewise.db`, and keyring entries. Running two copies at once corrupts the
//! SQLite upload queue and races config writes. This module enforces that only
//! one instance runs per OS user, and hands off "show the window" to the
//! already-running instance when a second launch is attempted.
//!
//! Mechanism: a local socket (named pipe on Windows, Unix-domain socket on
//! macOS/Linux) via the `interprocess` crate acts as BOTH the singleton lock
//! AND the IPC channel:
//!
//! - The first instance binds (listens on) the socket. The bind itself is the
//!   lock — a second bind to the same name fails with `AddrInUse`.
//! - A second instance, finding the name in use, connects to the existing
//!   socket, writes a fixed [`SHOW_MESSAGE`], and exits without launching a
//!   window. The first instance's accept loop receives that and raises its
//!   window.
//!
//! Robustness over strictness: any unexpected failure (permissions, a socket
//! error that isn't "name in use") degrades to launching normally rather than
//! refusing to start over a single-instance hiccup.

use std::io::{Read, Write};
use std::sync::OnceLock;

use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};
use interprocess::local_socket::{ListenerOptions, Name, Stream};
use tokio::sync::Notify;

/// Env var that, when set to a non-empty value, disables the guard entirely so
/// developers can run multiple instances side by side.
const ALLOW_MULTIPLE_ENV: &str = "LINEWISE_DESKTOP_ALLOW_MULTIPLE";

/// Fixed payload a second instance sends to the first to mean "show your
/// window". The content is ignored on receipt (any connection is the signal);
/// it exists only to make the protocol explicit and to give the writer
/// something to flush.
const SHOW_MESSAGE: &[u8] = b"SHOW\n";

/// Notifier the accept loop pokes when a second instance asks us to show.
/// A `use_future` in `app.rs` awaits [`show_requests`] and raises the window.
/// `Notify` is safe to signal from the plain `std::thread` accept loop and to
/// await from inside Dioxus's tokio runtime, which bridges the two cleanly.
static SHOW_NOTIFY: OnceLock<Notify> = OnceLock::new();

/// Returns the process-global notifier that fires once per "show" request.
///
/// Used by the accept loop (to signal) and by the UI hook (to await).
pub fn show_requests() -> &'static Notify {
    SHOW_NOTIFY.get_or_init(Notify::new)
}

/// Outcome of the startup guard, telling `main()` whether to keep going.
pub enum GuardOutcome {
    /// This is the first (or only) instance. Continue launching the UI. The
    /// accept loop has been spawned on a background thread.
    Continue,
    /// A live instance already owns the lock and has been signalled to show
    /// its window. The caller must exit without launching a UI.
    AlreadyRunning,
}

/// Per-user identity, fixed across debug and release builds.
///
/// Scoping by username keeps two OS users on a shared machine from colliding.
/// It is NEVER derived from the exe path or build profile, so a debug build and
/// a downloaded release map to the same lock — the entire point of the guard.
/// `whoami` isn't a dependency; we read the platform user env var and fall back
/// to a fixed string (degrading to machine-wide scope) when it's unavailable.
fn user_scope() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "default".to_owned())
}

/// The socket name plus, on Unix, the filesystem path backing it (so a stale
/// corpse can be unlinked). On Windows the path field is absent — named pipes
/// have no filesystem corpse to reclaim.
struct SocketName {
    name: Name<'static>,
    #[cfg(unix)]
    path: std::path::PathBuf,
}

/// Builds the socket name. Windows uses the namespaced primitive (named pipe);
/// Unix uses a filesystem-path socket under the temp dir so we own the path and
/// can reclaim a stale corpse directly.
#[cfg(windows)]
fn socket_name() -> std::io::Result<SocketName> {
    use interprocess::local_socket::{GenericNamespaced, ToNsName};
    let raw = format!("linewise-desktop-singleton-{}", user_scope());
    let name = raw.to_ns_name::<GenericNamespaced>()?;
    Ok(SocketName { name })
}

#[cfg(unix)]
fn socket_name() -> std::io::Result<SocketName> {
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    let file = format!("linewise-desktop-singleton-{}.sock", user_scope());
    let path = std::env::temp_dir().join(file);
    let name = path
        .clone()
        .into_os_string()
        .to_fs_name::<GenericFilePath>()?;
    Ok(SocketName { name, path })
}

/// Runs the single-instance guard. Call once in `main()` after config/logging
/// init and before launching the UI.
///
/// Returns [`GuardOutcome::Continue`] for the primary instance (and may have
/// spawned the accept loop) or [`GuardOutcome::AlreadyRunning`] when an existing
/// instance was signalled. Any internal error is logged and degrades to
/// [`GuardOutcome::Continue`] — we never refuse to start over a guard problem.
pub fn acquire() -> GuardOutcome {
    if std::env::var_os(ALLOW_MULTIPLE_ENV).is_some_and(|v| !v.is_empty()) {
        tracing::info!("{ALLOW_MULTIPLE_ENV} set — single-instance guard disabled");
        return GuardOutcome::Continue;
    }

    let socket = match socket_name() {
        Ok(socket) => socket,
        Err(e) => {
            tracing::warn!("single-instance: could not build socket name ({e}); launching anyway");
            return GuardOutcome::Continue;
        }
    };

    // Connect-first detection: if an instance is already listening, hand off to
    // it and exit. We probe by CONNECTING rather than interpreting a bind error,
    // because a failed bind reports platform-specific errors that don't map to a
    // single `ErrorKind` — notably, on Windows a name already held by another
    // named-pipe server returns ERROR_ACCESS_DENIED (os error 5 → PermissionDenied),
    // NOT `AddrInUse`, so the old bind-error sniffing silently let a 2nd instance
    // launch. A successful connect is an unambiguous "an instance is already here".
    if let Ok(mut stream) = Stream::connect(socket.name.clone()) {
        return match write_show(&mut stream) {
            Ok(()) => {
                tracing::info!(
                    "single-instance: existing instance found; signalled it to show; exiting"
                );
                GuardOutcome::AlreadyRunning
            }
            Err(e) => {
                tracing::warn!(
                    "single-instance: connected to existing instance but failed to signal ({e}); launching"
                );
                GuardOutcome::Continue
            }
        };
    }

    // Nobody answered — become the primary. On Unix a crashed instance can leave
    // a stale socket file that blocks bind; `bind_listener` reclaims it. On
    // Windows a name still held by a live server is detected via `name_in_use`.
    match bind_listener(&socket) {
        Ok(listener) => {
            tracing::info!("single-instance: acquired lock; this is the primary instance");
            spawn_accept_loop(listener);
            GuardOutcome::Continue
        }
        // Rare race: another instance bound between our connect probe and this
        // bind. A live instance holds the name — hand off and exit (fail-closed).
        Err(BindError::InUse) => signal_existing(socket.name),
        Err(BindError::Other(e)) => {
            // Unexpected bind error with no "in use" signal. Probe once for a
            // live instance: hand off if one answers, otherwise launch — we have
            // no evidence of a duplicate and must not brick a clean launch on a
            // transient/unknown error.
            tracing::warn!(
                "single-instance: bind failed unexpectedly ({e}); probing for an existing instance"
            );
            match Stream::connect(socket.name) {
                Ok(mut stream) => {
                    let _ = write_show(&mut stream);
                    tracing::info!("single-instance: an instance answered the probe; exiting");
                    GuardOutcome::AlreadyRunning
                }
                Err(_) => GuardOutcome::Continue,
            }
        }
    }
}

/// Why a bind attempt failed, distinguishing "another instance holds the name"
/// (the expected second-launch case) from any other I/O error (degrade).
enum BindError {
    /// The name is already in use by a live instance.
    InUse,
    /// Some other I/O failure — treat as a hiccup and degrade to launching.
    Other(std::io::Error),
}

/// Attempts to bind the listener, mapping `AddrInUse` to [`BindError::InUse`].
///
/// On Unix a crashed instance can leave a stale socket file that also yields
/// `AddrInUse`; [`reclaim_stale_socket`] connect-probes and unlinks the corpse
/// so we can rebind. On Windows named pipes are crash-safe — the name vanishes
/// when the owning process dies — so `AddrInUse` always means a live instance
/// and the reclaim path is a no-op.
fn bind_listener(socket: &SocketName) -> Result<interprocess::local_socket::Listener, BindError> {
    match try_bind(socket.name.clone()) {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if reclaim_stale_socket(socket) {
                return try_bind(socket.name.clone()).map_err(map_bind_error);
            }
            Err(BindError::InUse)
        }
        // Windows reports a name already owned by another named-pipe server as
        // ERROR_ACCESS_DENIED (5) / ERROR_PIPE_BUSY (231), which do NOT surface as
        // `AddrInUse`. Treat them as "in use" so the bind-race path hands off
        // instead of launching a duplicate.
        Err(e) if name_in_use(&e) => Err(BindError::InUse),
        Err(e) => Err(BindError::Other(e)),
    }
}

/// Whether a bind error means "another live server already owns this name",
/// covering the Windows error codes that don't map to `ErrorKind::AddrInUse`.
#[cfg(windows)]
fn name_in_use(e: &std::io::Error) -> bool {
    // ERROR_ACCESS_DENIED = 5, ERROR_PIPE_BUSY = 231.
    matches!(e.raw_os_error(), Some(5) | Some(231))
}

#[cfg(not(windows))]
fn name_in_use(_e: &std::io::Error) -> bool {
    false
}

/// Maps a raw bind I/O error to [`BindError`] for the post-reclaim retry.
fn map_bind_error(e: std::io::Error) -> BindError {
    if e.kind() == std::io::ErrorKind::AddrInUse {
        BindError::InUse
    } else {
        BindError::Other(e)
    }
}

/// Raw bind with name reclamation enabled (the `interprocess` default).
fn try_bind(name: Name<'static>) -> std::io::Result<interprocess::local_socket::Listener> {
    ListenerOptions::new()
        .name(name)
        .reclaim_name(true)
        .create_sync()
}

/// On Unix, probes whether an `AddrInUse` is a live instance or a stale corpse
/// left by a crash, and unlinks the corpse so the caller can rebind.
///
/// Returns `true` if a stale socket was reclaimed (caller should retry bind),
/// `false` if a live instance holds the name (caller should hand off + exit).
#[cfg(unix)]
fn reclaim_stale_socket(socket: &SocketName) -> bool {
    // A successful connect means a live instance is listening — not stale.
    if Stream::connect(socket.name.clone()).is_ok() {
        return false;
    }
    // Connect refused with the file present: it's a corpse from a crash. Unlink
    // it so the rebind succeeds.
    match std::fs::remove_file(&socket.path) {
        Ok(()) => {
            tracing::info!(
                "single-instance: reclaimed stale socket at {}",
                socket.path.display()
            );
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!("single-instance: could not reclaim stale socket: {e}");
            false
        }
    }
}

/// On Windows there is never a stale corpse to reclaim (named pipes are
/// crash-safe), so `AddrInUse` is always a live instance.
#[cfg(not(unix))]
fn reclaim_stale_socket(_socket: &SocketName) -> bool {
    false
}

/// Connect attempts (with backoff) when handing off to an existing instance.
/// Reached only after `bind` reported the name in use, so a live primary holds
/// it; the connect can transiently fail if that primary bound the name but
/// hasn't entered its accept loop yet (the connect→bind race).
const SIGNAL_CONNECT_ATTEMPTS: u32 = 5;
const SIGNAL_CONNECT_BACKOFF: std::time::Duration = std::time::Duration::from_millis(150);

/// Second-instance path: connect to the running instance, tell it to show, and
/// EXIT. Reached only after `bind` reported the name in use — on Windows a bound
/// name means a live process; on Unix any stale corpse was already reclaimed —
/// so a live primary exists and this process must NOT become a second uploader
/// (two instances share one SQLite upload queue → duplicate uploads).
///
/// A connect can transiently fail during the connect→bind race (primary bound
/// the name but isn't accepting yet); retry with backoff. If the hand-off still
/// can't be made we exit ANYWAY (fail-closed) — a missed "show the window" is
/// far better than a duplicate uploader.
fn signal_existing(name: Name<'static>) -> GuardOutcome {
    for attempt in 1..=SIGNAL_CONNECT_ATTEMPTS {
        match Stream::connect(name.clone()) {
            Ok(mut stream) => {
                match write_show(&mut stream) {
                    Ok(()) => tracing::info!(
                        "single-instance: signalled running instance to show; exiting"
                    ),
                    Err(e) => tracing::warn!(
                        "single-instance: connected but failed to signal ({e}); exiting (a live instance holds the lock)"
                    ),
                }
                return GuardOutcome::AlreadyRunning;
            }
            Err(e) => {
                tracing::debug!(
                    "single-instance: hand-off connect attempt {attempt}/{SIGNAL_CONNECT_ATTEMPTS} failed ({e}); retrying"
                );
                std::thread::sleep(SIGNAL_CONNECT_BACKOFF);
            }
        }
    }
    tracing::warn!(
        "single-instance: name in use but hand-off unreachable after {SIGNAL_CONNECT_ATTEMPTS} attempts; exiting rather than launching a duplicate"
    );
    GuardOutcome::AlreadyRunning
}

/// Writes the fixed show-message and flushes it.
fn write_show(stream: &mut Stream) -> std::io::Result<()> {
    stream.write_all(SHOW_MESSAGE)?;
    stream.flush()
}

/// Spawns the primary instance's background accept loop on a dedicated thread.
///
/// A plain `std::thread` with the blocking listener is the simplest robust
/// option: it doesn't depend on a running tokio reactor (Dioxus desktop spins
/// up its own runtime later), and the loop is pure blocking I/O. Each accepted
/// connection — regardless of payload — is one "show the window" request, which
/// we relay to the UI via [`show_requests`].
fn spawn_accept_loop(listener: interprocess::local_socket::Listener) {
    let spawned = std::thread::Builder::new()
        .name("single-instance-accept".to_owned())
        .spawn(move || accept_loop(listener));
    if let Err(e) = spawned {
        // On spawn failure the listener (moved into the closure) drops here,
        // releasing the lock. We accept this rare degradation: the primary
        // instance keeps running, it just loses exclusivity and the show
        // relay. Robustness over strictness — never abort startup over this.
        tracing::warn!("single-instance: could not spawn accept loop ({e}); guard degraded");
    }
}

/// Blocking accept loop. Runs for the lifetime of the process.
fn accept_loop(listener: interprocess::local_socket::Listener) {
    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => {
                drain(&mut stream);
                tracing::debug!("single-instance: show request received");
                show_requests().notify_one();
            }
            Err(e) => {
                // A single bad accept shouldn't kill the loop; keep serving.
                tracing::warn!("single-instance: accept error: {e}");
            }
        }
    }
}

/// Reads and discards the incoming payload. The connection itself is the
/// signal, so a read error is non-fatal — we still raise the window.
fn drain(stream: &mut Stream) {
    let mut buf = [0u8; SHOW_MESSAGE.len()];
    if let Err(e) = stream.read(&mut buf) {
        tracing::debug!("single-instance: ignoring read error on show request: {e}");
    }
}
