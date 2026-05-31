use agent_workspace::cli::{Cli, OutputFormat};
use agent_workspace::config::Config;
use agent_workspace::update;
use clap::Parser;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long `main` is willing to wait for the daily update check before
/// exiting. Kept well under the prompt-cache / human-perception threshold:
/// a warm check returns in a few ms; a cold/slow network is abandoned (the
/// detached thread finishes in the background) so the user never eats the
/// full HTTP timeout as tail latency.
const UPDATE_NOTICE_WAIT: Duration = Duration::from_millis(300);

fn main() {
    // Must be first: intercepts COMPLETE env var for shell completions
    clap_complete::env::CompleteEnv::with_factory(agent_workspace::cli::build_command).complete();

    let cli = Cli::parse();
    // Captured before `run` consumes `cli` so the error path can render
    // machine-readable errors in json mode.
    let format = cli.format();

    // Check for updates (once per day), runs in a background thread. Skipped for
    // the update-flow commands (`update`/`setup`) where the passive notice would
    // duplicate the command's own output — `ws update` re-execs `ws setup`, so
    // the notice could otherwise print twice in one run.
    let base_dir = Config::base_dir().ok();
    let update_rx = if cli.should_check_for_updates() {
        base_dir.as_ref().and_then(|dir| {
            if update::should_check(dir) {
                // Stamp the throttle marker EAGERLY (before the network call).
                // This guarantees the once/24h throttle holds even if the
                // request is slow or the process exits before the thread
                // finishes — a passive nag should fail quiet, not hammer
                // GitHub on every invocation.
                let _ = update::mark_checked(dir);
                Some(spawn_update_check())
            } else {
                None
            }
        })
    } else {
        None
    };

    let result = cli.run();

    // Surface the "new version available" notice only if the check already
    // finished (or finishes within a short window). We do NOT block on the
    // thread: if it's still talking to the network, we drop the receiver and
    // exit — the detached thread is reaped on process exit.
    // Suppress the passive nag in json mode to keep an agent's stderr clean.
    if !format.is_json()
        && let Some(rx) = update_rx
        && let Ok(Some(latest)) = rx.recv_timeout(UPDATE_NOTICE_WAIT)
    {
        eprintln!(
            "\x1b[33mA new version of agent-workspace is available: {VERSION} -> {latest}\x1b[0m"
        );
        eprintln!("\x1b[33mRun `ws update` to update\x1b[0m");
    }

    if let Err(e) = result {
        match format {
            OutputFormat::Json => print_error_json(&e),
            OutputFormat::Human => print_error_chain(&e),
        }
        std::process::exit(1);
    }
}

/// Render an error as a single JSON object on **stderr** (stdout stays clean so
/// a `--format json` pipeline isn't corrupted by a half-result). The `causes`
/// array mirrors the human cause-chain.
fn print_error_json(err: &dyn std::error::Error) {
    let mut causes = Vec::new();
    let mut source = err.source();
    while let Some(e) = source {
        causes.push(e.to_string());
        source = e.source();
    }
    let obj = serde_json::json!({ "error": err.to_string(), "causes": causes });
    match serde_json::to_string_pretty(&obj) {
        Ok(s) => eprintln!("{s}"),
        Err(_) => eprintln!("{{\"error\":{:?}}}", err.to_string()),
    }
}

/// Spawn the update check on a detached thread. Returns a receiver that yields
/// `Some(latest_version)` when a newer release exists, `None` otherwise. The
/// throttle marker is written by the caller before spawning, so this thread
/// only performs the network check.
fn spawn_update_check() -> Receiver<Option<String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let latest = update::check_update(VERSION).ok().flatten();
        // If the receiver was already dropped (we timed out and exited), this
        // send fails harmlessly.
        let _ = tx.send(latest);
    });
    rx
}

/// Print an error and its `source()` chain to stderr. Typed `thiserror` enums
/// wire `#[from]`/`#[source]`, so a wrapped IO/parse failure surfaces its root
/// cause here instead of being swallowed behind the top-level `Display`. The
/// chain is capped to keep noise down unless `WS_DEBUG` is set.
fn print_error_chain(err: &dyn std::error::Error) {
    eprintln!("error: {err}");
    let verbose = std::env::var_os("WS_DEBUG").is_some();
    let mut source = err.source();
    let mut depth = 0;
    while let Some(e) = source {
        eprintln!("  caused by: {e}");
        source = e.source();
        depth += 1;
        if !verbose && depth >= 3 {
            break;
        }
    }
}
