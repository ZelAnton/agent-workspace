use agent_workspace::cli::Cli;
use agent_workspace::config::Config;
use agent_workspace::update;
use clap::Parser;
use std::thread::JoinHandle;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    // Must be first: intercepts COMPLETE env var for shell completions
    clap_complete::env::CompleteEnv::with_factory(agent_workspace::cli::build_command).complete();

    let cli = Cli::parse();

    // Check for updates (once per day), runs in background. Skipped for the
    // update-flow commands (`update`/`setup`) where the passive notice would
    // duplicate the command's own output — `ws update` re-execs `ws setup`,
    // so the notice could otherwise print twice in one run.
    let base_dir = Config::base_dir().ok();
    let update_handle = if cli.should_check_for_updates() {
        base_dir.as_ref().and_then(|dir| {
            if update::should_check(dir) {
                Some(spawn_update_check(dir.clone()))
            } else {
                None
            }
        })
    } else {
        None
    };

    let result = cli.run();

    // Wait for update check to complete before exiting
    if let Some(handle) = update_handle {
        let _ = handle.join();
    }

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn spawn_update_check(base_dir: std::path::PathBuf) -> JoinHandle<()> {
    std::thread::spawn(move || {
        if let Ok(Some(latest)) = update::check_update(VERSION) {
            eprintln!(
                "\x1b[33mA new version of agent-workspace is available: {} -> {}\x1b[0m",
                VERSION, latest
            );
            eprintln!("\x1b[33mRun `ws update` to update\x1b[0m");
        }
        // Mark that we checked (ignore errors)
        let _ = update::mark_checked(&base_dir);
    })
}
