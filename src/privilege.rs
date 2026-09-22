/// Check whether the current process is running as root.
pub fn is_root() -> bool {
    // SAFETY: getuid() is async-signal-safe and has no failure mode.
    unsafe { libc::getuid() == 0 }
}
