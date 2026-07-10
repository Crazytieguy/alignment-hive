//! Embedded machine-side lifecycle scripts for later runtime integration.

/// Fenced lease state-machine script.
pub const LEASE: &str = include_str!("../scripts/machine/rk-lease.sh");

/// Detached Jupyter drain and finalize supervisor script.
pub const WATCHDOG: &str = include_str!("../scripts/machine/rk-watchdog.sh");

/// The caller no longer owns the lease generation or operation.
pub const EXIT_FENCED: i32 = 9;
/// The terminal `finalizing` state refuses the requested operation.
pub const EXIT_REFUSED: i32 = 10;
/// The operation, arguments, or persisted lease are invalid.
pub const EXIT_INVALID: i32 = 11;
/// Bootstrap cannot find the required util-linux `flock` binary.
pub const EXIT_NO_FLOCK: i32 = 12;
/// Bootstrap cannot use the persistent state directory.
pub const EXIT_BAD_STATE_DIR: i32 = 13;
/// Bootstrap cannot find another required machine-side dependency.
pub const EXIT_MISSING_DEPENDENCY: i32 = 14;
