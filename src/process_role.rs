//! Detect whether the current process is the **primary Zed UI** or a
//! **helper sub-process** spawned by a primary.
//!
//! ## Why
//!
//! The hook dylib is patched into `/Applications/Zed Preview.app/Contents/MacOS/zed`,
//! which means *every* `MacOS/zed` invocation loads it — including all the
//! helper sub-processes Zed spawns for itself (LSP wrappers, file scanners,
//! crash handler, language-server processes, ACP agent helpers, etc.).
//!
//! On Zed v1.1.2 the per-launch helper count grew to ~9-10. Running the
//! full ctor (Gum::obtain, 5 sequential symbol scans across a 350MB
//! binary, file-backed registry mutex via `locked_register`) in every
//! one of them caused ~53-second startup hangs as the helpers serialised
//! on dyld + the cross-process registry lock.
//!
//! In a helper process, *none* of the YOLO machinery is needed:
//! - The auto-approve hooks only fire in the primary UI process where
//!   the user sees tool-permission prompts.
//! - We don't need to register in the hook registry.
//! - We don't need the periodic stale scanner thread.
//!
//! ## How we detect
//!
//! Heuristic: walk up the process tree (parent, grandparent, …) up to
//! `MAX_ANCESTORS` hops. If *any* ancestor's executable path matches
//! ours, we're a descendant of another `MacOS/zed` instance and should
//! classify as Helper. Otherwise — Primary.
//!
//! Just checking `getppid()` is not enough. Zed v1.1.2 spawns helpers
//! through non-zed intermediates: e.g. main zed → Node (Claude-Code
//! ACP host) → grandchild zed-cli. The grandchild zed loads our dylib,
//! but its direct parent is Node, not zed — so a one-hop check
//! mis-classifies it as Primary. (Empirically observed 2026-04-30:
//! after a one-hop guard ~10 helpers per launch still ran full init,
//! ppids like 95262, 95323, 95325, … none of which were zed binaries.)
//! Walking up the tree catches the grandchild case as long as one
//! ancestor within the window is a zed binary.
//!
//! Edge cases (all fail-open as "primary"):
//! - `ppid <= 1` → parent is launchd → primary.
//! - `proc_pidpath` fails at any hop → can't read ancestor → primary.
//! - `current_exe()` fails → no comparator → primary.
//! - Walked `MAX_ANCESTORS` hops without finding a zed → primary.
//!
//! Failing open is the safe default: the worst case is we run full init
//! in a helper (the pre-fix behaviour), not silently break the main UI
//! process.
//!
//! Mirrors the same module in `zed-prj-workspace-hook` (commit 2ee696e,
//! 2026-04-23), with the multi-hop walk added on 2026-04-30 to handle
//! Zed v1.1.2's deeper helper trees.

use std::ffi::CStr;
use std::path::PathBuf;

/// Maximum number of ancestor hops we walk up the process tree.
///
/// 5 covers all observed Zed helper chains (main → cli-shim → node →
/// zed-cli is 3 hops; the deepest seen so far is ~4) with margin to
/// spare. Going higher costs a `proc_pidinfo` syscall per hop and risks
/// crossing into the user's shell / terminal ancestry where we'd
/// definitely *not* want to apply the helper rule.
const MAX_ANCESTORS: usize = 5;

/// What kind of process are we?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRole {
    /// First-class UI / CLI-launched process. Runs full hook init.
    Primary,
    /// Spawned by another `MacOS/zed` process (helper sub-mode, language
    /// server wrapper, etc.). Skips heavy init.
    Helper,
}

impl ProcessRole {
    pub fn is_helper(self) -> bool {
        matches!(self, ProcessRole::Helper)
    }
}

/// Detect this process's role by walking up the process tree looking
/// for an ancestor whose executable matches ours.
///
/// Best-effort and side-effect free; safe to call from `#[ctor]`.
pub fn detect() -> ProcessRole {
    let our_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return ProcessRole::Primary,
    };

    let mut pid = unsafe { libc::getppid() };
    for _ in 0..MAX_ANCESTORS {
        // launchd is pid 1; we've reached the root without finding zed.
        if pid <= 1 {
            return ProcessRole::Primary;
        }
        match parent_executable_path(pid) {
            Some(exe) if exe == our_exe => return ProcessRole::Helper,
            Some(_) => {
                // Different binary at this hop. Walk further up.
                match parent_pid_of(pid) {
                    Some(p) => pid = p,
                    None => return ProcessRole::Primary,
                }
            }
            None => {
                // Couldn't read this ancestor's exe (process gone,
                // permission denied, …). Fail open — assume primary.
                return ProcessRole::Primary;
            }
        }
    }
    // Walked the window without finding zed → primary.
    ProcessRole::Primary
}

/// Read the executable path of `pid` via macOS `proc_pidpath(2)`.
///
/// Returns `None` if the syscall fails (process gone, permission denied,
/// non-macOS, …).
pub fn parent_executable_path(pid: libc::pid_t) -> Option<PathBuf> {
    const BUF_SIZE: usize = libc::PROC_PIDPATHINFO_MAXSIZE as usize;
    let mut buf = [0u8; BUF_SIZE];

    // SAFETY: `proc_pidpath` writes a NUL-terminated C string up to
    // `buf_size` bytes into `buf`. Returns the number of bytes written
    // (excluding NUL) on success, -1 on failure.
    let n =
        unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut libc::c_void, BUF_SIZE as u32) };
    if n <= 0 {
        return None;
    }
    // The returned length excludes the NUL terminator, but the buffer is
    // NUL-terminated by the call. Use CStr to be defensive.
    let cstr = CStr::from_bytes_until_nul(&buf).ok()?;
    let path_str = cstr.to_str().ok()?;
    Some(PathBuf::from(path_str))
}

/// Look up the parent PID of an arbitrary `pid` via
/// `proc_pidinfo(PROC_PIDTBSDINFO)`.
///
/// Returns `None` if the syscall fails or the returned struct is short.
/// `getppid()` (libc) only works for the *current* process — we need
/// this to walk further ancestors.
pub fn parent_pid_of(pid: libc::pid_t) -> Option<libc::pid_t> {
    // Use libc's platform definition rather than duplicating the C layout.
    // This keeps the buffer aligned with the active macOS SDK/libc crate.
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;

    // SAFETY: We pass a sized struct buffer matching the kernel's
    // expectation for `PROC_PIDTBSDINFO`. The syscall returns the number
    // of bytes written.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr() as *mut libc::c_void,
            size,
        )
    };
    if n < size {
        return None;
    }
    // SAFETY: a full `proc_bsdinfo` was written when `n >= size`.
    let info = unsafe { info.assume_init() };
    Some(info.pbi_ppid as libc::pid_t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_enum_helpers() {
        assert!(ProcessRole::Helper.is_helper());
        assert!(!ProcessRole::Primary.is_helper());
    }

    #[test]
    fn detect_returns_primary_for_test_process() {
        // `cargo test` runs us under the test harness, whose parent is
        // typically the cargo / shell process — definitely not the same
        // executable as us. Should classify as Primary.
        let role = detect();
        assert_eq!(role, ProcessRole::Primary);
    }

    #[test]
    fn parent_executable_path_for_self() {
        // We can read OUR OWN executable path via the same syscall.
        let our_pid = unsafe { libc::getpid() };
        let path = parent_executable_path(our_pid).expect("can read own exe path");
        // The result should be a non-empty existing path.
        assert!(path.exists(), "exe path should exist: {}", path.display());
    }

    #[test]
    fn parent_executable_path_for_pid1_is_launchd() {
        // pid 1 on macOS is launchd. We may not have permission to read
        // its path on hardened runtimes, but the call should not panic.
        let _result = parent_executable_path(1);
    }

    #[test]
    fn parent_pid_of_self_matches_getppid() {
        // proc_pidinfo on our own pid should report a ppid that matches
        // libc::getppid().
        let our_pid = unsafe { libc::getpid() };
        let expected = unsafe { libc::getppid() };
        let got = parent_pid_of(our_pid).expect("can read own bsdinfo");
        assert_eq!(got, expected);
    }

    #[test]
    fn parent_pid_of_pid1_is_zero_or_one() {
        // launchd's parent is itself / kernel; the syscall may report 0
        // or 1, both acceptable. Just don't panic and don't return giant
        // garbage.
        if let Some(pp) = parent_pid_of(1) {
            assert!(pp <= 1, "launchd should have pid 0 or 1 as parent");
        }
    }
}
