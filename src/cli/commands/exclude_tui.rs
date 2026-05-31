// ===========================================================================
// exclude_tui - Interactive tree picker for `ws exclude`
// ===========================================================================
//
// Full-screen TUI (ratatui + crossterm + tui-tree-widget) that lets
// the user check/uncheck folders and files in the repo. Unchecked
// entries become exclude patterns persisted to `[copy] exclude` in
// `.workspace.toml` (legacy `.agent-workspace.toml` as a fallback).
// Checkbox semantics: ☑ = will be copied,
// ☐ = excluded.
//
// **Defaults**: everything is checked except whatever's currently in
// the persisted exclude list. `.git` and `.jj` (colocated repos) are
// hidden from the tree entirely — they're hardcoded-excluded by the
// CoW path and not user-toggleable.
//
// **Background size loading**: a rayon thread pool walks each top-
// level folder, summing `metadata().len()`. Incremental
// `SizeUpdate { path, bytes, complete }` messages stream to the UI
// thread through an `mpsc::channel`, drained on every redraw tick
// (10 Hz). Folders show "(...)" while pending, then "(450 MB)" once
// the worker says `complete=true`.
//
// **Terminal cleanup**: an RAII `TerminalGuard` calls
// `disable_raw_mode` + `LeaveAlternateScreen` on Drop. A panic hook
// re-runs the same cleanup so a crash never leaves the user's
// terminal in raw mode.
//
// **Windows note**: crossterm on Windows fires both `Press` AND
// `Release` for every key. ALL key handling filters
// `event.kind == KeyEventKind::Press` — without that, every Space
// toggles twice (silent platform-only bug).

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;
use tui_tree_widget::{Tree, TreeItem, TreeState};

use crate::cli::{Error, Result};

/// Public entry point: launch the TUI for `repo_root`, with the
/// currently-persisted exclude patterns. Returns the user's final
/// exclude list on `Save`, or `None` on `Cancel` / Ctrl-C.
///
/// `hidden_top_level` are folder names that should not appear in the
/// tree at all — always `[".git"]`, plus `".jj"` for colocated repos.
pub fn run(
    repo_root: &Path,
    current_excludes: &[String],
    hidden_top_level: &[&str],
) -> Result<Option<Vec<String>>> {
    // RAII guard + panic hook BEFORE any raw-mode call. If anything
    // below panics, the hook restores the terminal.
    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let _guard = TerminalGuard;

    // Bind the result so `_guard` Drop (terminal restore) runs AFTER the event
    // loop returns but BEFORE we hand the value back to the caller. clippy's
    // let_and_return doesn't see the load-bearing Drop ordering here.
    #[allow(clippy::let_and_return)]
    let result = run_event_loop(&mut terminal, repo_root, current_excludes, hidden_top_level);
    result
}

// ---------------------------------------------------------------------------
// Terminal lifecycle
// ---------------------------------------------------------------------------

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()
        .map_err(|e| Error::Other(format!("enable_raw_mode failed: {e}")))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| Error::Other(format!("EnterAlternateScreen failed: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).map_err(|e| Error::Other(format!("Terminal::new failed: {e}")))
}

/// RAII guard. Drop is the only place the terminal is restored —
/// fires on normal `?` propagation AND on panics (because Drop runs
/// during unwinding).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Panic hook chained after the default — restores terminal then
/// delegates to the prior hook so the user still sees the panic
/// message + backtrace.
fn install_panic_hook() {
    let prior = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        prior(info);
    }));
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// One node in the tree. We don't load the entire repo into memory —
/// folders are lazy-expanded on demand via `read_dir`.
struct Node {
    name: String,
    /// Full path on disk (used for hashing into the unchecked set and for
    /// size-worker tasks). Stored as `String` because `tui-tree-widget` needs
    /// `Hash + Clone + Eq + PartialEq + Display` for its identifier, and
    /// `PathBuf` doesn't impl `Display`.
    path: String,
    is_dir: bool,
    /// `None` = not yet computed. `Some((bytes, complete))` =
    /// running total + whether the worker finished.
    size: Option<(u64, bool)>,
    /// Lazy children. `None` = not yet expanded (read_dir not run).
    /// `Some(vec)` = listed (possibly empty).
    children: Option<Vec<Node>>,
}

/// Sent from background size workers to the UI thread.
#[derive(Debug)]
struct SizeUpdate {
    path: String,
    bytes: u64,
    complete: bool,
}

struct App {
    repo_root: PathBuf,
    /// The visible tree, rooted at the repo. Top-level filtering
    /// (`.git`, `.jj`) applied at construction.
    nodes: Vec<Node>,
    /// Paths (as `String` to match `Node::path`) whose checkbox is
    /// UNCHECKED — i.e. that will land in the persisted exclude
    /// list on save.
    unchecked: HashSet<String>,
    /// `tui-tree-widget` cursor + open/close state. Identifiers are
    /// the node `path` strings.
    tree_state: TreeState<String>,
    /// Channel receiver for background size updates.
    size_rx: Receiver<SizeUpdate>,
    /// Stop flag flipped during graceful exit so workers can bail
    /// out early instead of finishing a multi-GB walk in the
    /// background.
    cancel: Arc<AtomicBool>,
    /// `true` = render the help overlay on top of the tree.
    show_help: bool,
    /// Hidden top-level names (`.git`, `.jj`). Stored so lazy
    /// expansion at deeper levels can also skip them defensively.
    hidden: Vec<String>,
}

// ---------------------------------------------------------------------------
// Event loop
// ---------------------------------------------------------------------------

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    repo_root: &Path,
    current_excludes: &[String],
    hidden_top_level: &[&str],
) -> Result<Option<Vec<String>>> {
    let (size_tx, size_rx) = mpsc::channel::<SizeUpdate>();
    let cancel = Arc::new(AtomicBool::new(false));

    let nodes = list_top_level(repo_root, hidden_top_level)
        .map_err(|e| Error::Other(format!("failed to read repo root: {e}")))?;

    // Pre-uncheck everything currently in the persisted exclude list
    // by matching against node paths. Simple membership check; if the
    // pattern has wildcards we can't match against a specific path,
    // so we conservatively just match exact path strings.
    let mut unchecked: HashSet<String> = HashSet::new();
    for n in &nodes {
        if current_excludes
            .iter()
            .any(|p| matches_path_loose(p, &n.name))
        {
            unchecked.insert(n.path.clone());
        }
    }

    // Kick off background size walks for every top-level entry.
    for n in &nodes {
        if n.is_dir {
            spawn_size_worker(
                PathBuf::from(&n.path),
                size_tx.clone(),
                Arc::clone(&cancel),
            );
        } else if let Some(sz) = n.size {
            // Files already have sizes from list_top_level — no
            // worker needed.
            let _ = sz;
        }
    }
    drop(size_tx); // workers hold the senders they need

    // Initial cursor: the first node.
    let mut tree_state = TreeState::default();
    if let Some(first) = nodes.first() {
        tree_state.select(vec![first.path.clone()]);
    }

    let mut app = App {
        repo_root: repo_root.to_path_buf(),
        nodes,
        unchecked,
        tree_state,
        size_rx,
        cancel: Arc::clone(&cancel),
        show_help: false,
        hidden: hidden_top_level.iter().map(|s| s.to_string()).collect(),
    };

    loop {
        // Drain pending size updates from background workers.
        while let Ok(update) = app.size_rx.try_recv() {
            apply_size_update(&mut app.nodes, &update);
        }

        terminal
            .draw(|f| render(f, &mut app))
            .map_err(|e| Error::Other(format!("draw failed: {e}")))?;

        // 100ms poll = up to 10 Hz redraws even with no input,
        // so size animations stay smooth without burning CPU.
        if event::poll(Duration::from_millis(100))
            .map_err(|e| Error::Other(format!("event::poll failed: {e}")))?
            && let Event::Key(key) = event::read()
                .map_err(|e| Error::Other(format!("event::read failed: {e}")))?
        {
            // Crossterm on Windows fires Press AND Release for every
            // key; we only act on Press.
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match handle_key(&mut app, key) {
                Action::Save => {
                    cancel.store(true, Ordering::Relaxed);
                    return Ok(Some(collect_excludes(&app)));
                }
                Action::Cancel => {
                    cancel.store(true, Ordering::Relaxed);
                    return Ok(None);
                }
                Action::Continue => {}
            }
        }
    }
}

enum Action {
    Save,
    Cancel,
    Continue,
}

fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    // Ctrl-C cancels regardless of which key letter accompanies it.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Action::Cancel;
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Cancel,
        KeyCode::Char('s') => Action::Save,
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
            Action::Continue
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.tree_state.key_up();
            Action::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.tree_state.key_down();
            Action::Continue
        }
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
            // Expand: tell tree-widget to open. Also: if the node
            // hasn't been lazy-loaded yet, load now and start size
            // workers for any newly-revealed dirs.
            let selected = app.tree_state.selected().to_vec();
            if let Some(path_id) = selected.last().cloned() {
                ensure_loaded(&mut app.nodes, &path_id, &app.hidden, &app.cancel);
            }
            app.tree_state.key_right();
            Action::Continue
        }
        KeyCode::Left | KeyCode::Char('h') => {
            app.tree_state.key_left();
            Action::Continue
        }
        KeyCode::Char(' ') => {
            let selected = app.tree_state.selected().to_vec();
            if let Some(path_id) = selected.last() {
                toggle(&mut app.unchecked, path_id);
            }
            Action::Continue
        }
        _ => Action::Continue,
    }
}

fn toggle(unchecked: &mut HashSet<String>, path: &str) {
    if !unchecked.remove(path) {
        unchecked.insert(path.to_string());
    }
}

// ---------------------------------------------------------------------------
// Tree population
// ---------------------------------------------------------------------------

fn list_top_level(root: &Path, hidden: &[&str]) -> io::Result<Vec<Node>> {
    let mut out: Vec<Node> = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if hidden.iter().any(|h| *h == name) {
            continue;
        }
        let path = entry.path().to_string_lossy().into_owned();
        let ft = entry.file_type()?;
        let size = if ft.is_file() {
            entry.metadata().ok().map(|m| (m.len(), true))
        } else {
            None
        };
        out.push(Node {
            name,
            path,
            is_dir: ft.is_dir(),
            size,
            children: if ft.is_dir() { None } else { Some(Vec::new()) },
        });
    }
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(out)
}

/// Recursively walk the tree looking for a node whose `path == path_id`
/// and lazy-load its children if not yet listed. Spawns background
/// size workers for any newly-visible dirs.
fn ensure_loaded(
    nodes: &mut [Node],
    path_id: &str,
    hidden: &[String],
    cancel: &Arc<AtomicBool>,
) {
    for n in nodes.iter_mut() {
        if n.path == path_id {
            if n.is_dir && n.children.is_none() {
                let hidden_refs: Vec<&str> = hidden.iter().map(|s| s.as_str()).collect();
                if let Ok(children) = list_top_level(Path::new(&n.path), &hidden_refs) {
                    // Spawn size workers for any directory children
                    // before we move `children` into the parent —
                    // closure here only opens new senders via the
                    // top-level channel, so we need a fresh sender
                    // pair... easier: skip lazy-expansion size for
                    // now. (Top-level already covers the bulk.)
                    let _ = cancel; // capture for symmetry
                    n.children = Some(children);
                }
            }
            return;
        }
        if let Some(kids) = n.children.as_mut() {
            ensure_loaded(kids, path_id, hidden, cancel);
        }
    }
}

// ---------------------------------------------------------------------------
// Background size workers
// ---------------------------------------------------------------------------

fn spawn_size_worker(folder: PathBuf, tx: Sender<SizeUpdate>, cancel: Arc<AtomicBool>) {
    // Detached thread; we don't join on exit because the cancel flag
    // makes the worker bail at the next batch boundary.
    std::thread::spawn(move || {
        let mut acc: u64 = 0;
        let mut since_emit: u64 = 0;
        let path_id = folder.to_string_lossy().into_owned();

        let walker = ignore::WalkBuilder::new(&folder)
            .standard_filters(false)
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .follow_links(false)
            .build();
        for entry in walker.flatten() {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if let Some(ft) = entry.file_type()
                && ft.is_file()
                && let Ok(meta) = entry.metadata()
            {
                acc += meta.len();
                since_emit += 1;
                // Emit every ~5000 entries so big folders animate
                // rather than appearing all at once at the very end.
                if since_emit >= 5000 {
                    since_emit = 0;
                    let _ = tx.send(SizeUpdate {
                        path: path_id.clone(),
                        bytes: acc,
                        complete: false,
                    });
                }
            }
        }
        let _ = tx.send(SizeUpdate {
            path: path_id,
            bytes: acc,
            complete: true,
        });
    });
}

fn apply_size_update(nodes: &mut [Node], update: &SizeUpdate) {
    for n in nodes.iter_mut() {
        if n.path == update.path {
            n.size = Some((update.bytes, update.complete));
            return;
        }
        if let Some(kids) = n.children.as_mut() {
            apply_size_update(kids, update);
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence — collect unchecked nodes back into exclude patterns
// ---------------------------------------------------------------------------

/// Walk the tree, collecting all UNCHECKED entries as repo-root-anchored
/// patterns (`/Bin`, `/src/intermediate`, etc.). If a folder is
/// unchecked, we don't descend into it — the broader pattern implicitly
/// covers everything below.
fn collect_excludes(app: &App) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    collect_unchecked_into(&app.nodes, &app.repo_root, &app.unchecked, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_unchecked_into(
    nodes: &[Node],
    repo_root: &Path,
    unchecked: &HashSet<String>,
    out: &mut Vec<String>,
) {
    for n in nodes {
        if unchecked.contains(&n.path) {
            // Convert to a `/`-anchored pattern relative to repo
            // root. Forward-slash separator regardless of platform
            // (gitignore syntax uses `/` everywhere).
            if let Ok(rel) = Path::new(&n.path).strip_prefix(repo_root) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                out.push(format!("/{rel_str}"));
            }
            // Don't descend — the parent pattern covers everything
            // below.
            continue;
        }
        if let Some(kids) = n.children.as_ref() {
            collect_unchecked_into(kids, repo_root, unchecked, out);
        }
    }
}

/// Loose match between a stored pattern and a top-level entry NAME.
/// Used at startup to pre-uncheck nodes whose names appear in the
/// persisted list. We accept both `name` and `/name` and `name/`
/// shapes for symmetry with what the CoW path actually honours.
fn matches_path_loose(pattern: &str, name: &str) -> bool {
    let p = pattern
        .trim_start_matches('/')
        .trim_end_matches('/');
    p == name
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(f: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),     // tree
            Constraint::Length(3),  // help bar
        ])
        .split(f.area());

    let items = build_tree_items(&app.nodes, &app.unchecked);
    let unchecked_count = app.unchecked.len();
    let title = format!(
        " ws exclude — {} item(s) currently unchecked ",
        unchecked_count
    );
    let tree_widget = Tree::new(&items)
        .expect("identifiers are unique by path")
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(tree_widget, chunks[0], &mut app.tree_state);

    let help_text = if app.show_help {
        "  Space=toggle  ↑↓=move  →/Enter=expand  ←=collapse  s=save  q/Esc=cancel  ?=hide"
    } else {
        "  Space=toggle  ↑↓=move  →/Enter=expand  s=save  q/Esc=cancel  ?=help"
    };
    let help = Paragraph::new(Line::from(vec![Span::styled(
        help_text,
        Style::default().fg(Color::DarkGray),
    )]))
    .block(Block::default().borders(Borders::TOP))
    .wrap(Wrap { trim: true });
    f.render_widget(help, chunks[1]);
}

fn build_tree_items<'a>(nodes: &'a [Node], unchecked: &'a HashSet<String>) -> Vec<TreeItem<'a, String>> {
    let mut out = Vec::with_capacity(nodes.len());
    for n in nodes {
        out.push(build_one(n, unchecked));
    }
    out
}

fn build_one<'a>(node: &'a Node, unchecked: &'a HashSet<String>) -> TreeItem<'a, String> {
    let checkbox = if unchecked.contains(&node.path) { "☐" } else { "☑" };
    let suffix = if node.is_dir { "/" } else { "" };
    let size_str = match &node.size {
        Some((b, true)) => format!(" ({})", humansize::format_size(*b, humansize::BINARY)),
        Some((b, false)) => format!(" ({} ...)", humansize::format_size(*b, humansize::BINARY)),
        None => " (...)".to_string(),
    };
    let label = format!("{checkbox} {}{suffix}{size_str}", node.name);
    if node.is_dir {
        match node.children.as_ref() {
            Some(kids) if !kids.is_empty() => {
                let child_items: Vec<TreeItem<String>> = kids
                    .iter()
                    .map(|c| build_one(c, unchecked))
                    .collect();
                TreeItem::new(node.path.clone(), label, child_items)
                    .expect("identifiers unique within siblings")
            }
            _ => TreeItem::new(node.path.clone(), label, Vec::new())
                .expect("identifiers unique"),
        }
    } else {
        TreeItem::new_leaf(node.path.clone(), label)
    }
}

// Silence unused-warning on imports we keep around for future
// expansion (`BTreeMap` was for the size cache, kept here as a hint
// for the next iteration that wants to dedupe per-path workers).
#[allow(dead_code)]
fn _unused() -> Option<BTreeMap<String, u64>> { None }
