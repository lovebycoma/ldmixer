//! lD Mixer: an unofficial Linux mixer for Audient iD interfaces.
//!
//! The library holds everything that is not UI: the protocol tables, the USB
//! transport (and a mock device), the mixer state model, persistence and the
//! background device engine.

/// Application ID: GTK application, desktop file, icon and tray name.
pub const APP_ID: &str = "com.lovebycoma.ldmixer";

pub mod config;
pub mod device;
pub mod engine;
pub mod ipc;
pub mod daemon;
pub mod tray;
pub mod mixer;
pub mod mock;
pub mod protocol;
pub mod vdev;

/// For `pre_exec`: give a child process a clean signal mask. The daemon
/// blocks SIGTERM/SIGINT in its own threads, and a blocked mask would be
/// inherited across exec, making children ignore those signals (including
/// the parent-death signal that stops virtual devices).
///
/// # Safety
/// Only call between fork and exec; it uses async-signal-safe calls only.
pub unsafe fn reset_signal_mask() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
    }
}
