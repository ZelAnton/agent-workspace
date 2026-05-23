// ===========================================================================
// terminal/tests - Cross-backend smoke tests
// ===========================================================================
//
// Per-backend logic (Windows-Terminal arg layout, AppleScript escaping,
// POSIX shell quoting) lives in each backend's `#[cfg(test)]` block.
// This file pins the public surface: env-detection contract,
// SPAWNED_IN_TAB_ENV recursion guard, fall-through behaviour.
//
// **Concurrency**: env-var mutation is process-global. Cargo runs tests
// in parallel by default, so these tests serialise on `ENV_MUTEX` to
// prevent one test scrubbing another's setup. Without it, flaky
// failures depend on thread scheduling.

use std::sync::Mutex;

use super::*;

static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Scrub all known terminal env vars + take the global mutex so no
/// other test can race-mutate during `f`. Poisoning is irrelevant
/// (we don't leak invariants across tests) so we recover via
/// `into_inner`.
fn with_clean_env<F: FnOnce()>(f: F) {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: ENV_MUTEX ensures no other test mutates env concurrently.
    // The mutation IS process-global, but we control all readers (this
    // module's tests) via the mutex.
    unsafe {
        std::env::remove_var("WT_SESSION");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("GNOME_TERMINAL_SERVICE");
        std::env::remove_var("GNOME_TERMINAL_SCREEN");
        std::env::remove_var(SPAWNED_IN_TAB_ENV);
    }
    f();
}

#[test]
fn detect_returns_none_when_no_terminal_env() {
    with_clean_env(|| {
        let result = detect();
        assert!(result.is_none(), "expected None when no terminal env set");
    });
}

#[test]
fn is_spawned_in_tab_false_by_default() {
    with_clean_env(|| {
        assert!(!is_spawned_in_tab());
    });
}

#[test]
fn is_spawned_in_tab_true_when_env_set() {
    with_clean_env(|| {
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var(SPAWNED_IN_TAB_ENV, "1");
        }
        assert!(is_spawned_in_tab());
        unsafe {
            std::env::remove_var(SPAWNED_IN_TAB_ENV);
        }
    });
}

#[test]
fn is_spawned_in_tab_false_when_env_empty() {
    with_clean_env(|| {
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var(SPAWNED_IN_TAB_ENV, "");
        }
        assert!(!is_spawned_in_tab(), "empty string should not count as spawned");
        unsafe {
            std::env::remove_var(SPAWNED_IN_TAB_ENV);
        }
    });
}

#[test]
fn detect_finds_windows_terminal_via_wt_session() {
    with_clean_env(|| {
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var("WT_SESSION", "abc-123");
        }
        let result = detect();
        assert!(result.is_some());
        assert_eq!(result.unwrap().name(), "windows-terminal");
        unsafe {
            std::env::remove_var("WT_SESSION");
        }
    });
}

#[test]
fn detect_finds_iterm2_via_term_program() {
    with_clean_env(|| {
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var("TERM_PROGRAM", "iTerm.app");
        }
        let result = detect();
        assert!(result.is_some());
        assert_eq!(result.unwrap().name(), "iterm2");
        unsafe {
            std::env::remove_var("TERM_PROGRAM");
        }
    });
}

#[test]
fn detect_finds_gnome_terminal_via_service_env() {
    with_clean_env(|| {
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var("GNOME_TERMINAL_SERVICE", ":1.42");
        }
        let result = detect();
        assert!(result.is_some());
        assert_eq!(result.unwrap().name(), "gnome-terminal");
        unsafe {
            std::env::remove_var("GNOME_TERMINAL_SERVICE");
        }
    });
}

#[test]
fn detect_prefers_windows_terminal_when_multiple_envs_set() {
    with_clean_env(|| {
        // SAFETY: single-threaded test.
        unsafe {
            std::env::set_var("WT_SESSION", "abc");
            std::env::set_var("TERM_PROGRAM", "iTerm.app");
        }
        let result = detect();
        assert!(result.is_some());
        // Detection order is WT → iTerm2 → GNOME Terminal.
        assert_eq!(result.unwrap().name(), "windows-terminal");
        unsafe {
            std::env::remove_var("WT_SESSION");
            std::env::remove_var("TERM_PROGRAM");
        }
    });
}
