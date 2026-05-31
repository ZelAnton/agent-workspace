// ===========================================================================
// cli/output - Output format + the human/JSON rendering choke point
// ===========================================================================
//
// `ws` is driven by AI agents as well as humans, so the read/result commands
// can emit a stable machine-readable object instead of scraped text.
//
// **stdout / stderr discipline** (enforced by routing all payload through
// `emit`):
//   - **stdout** carries the ONE machine payload per invocation — the
//     human-formatted block in `human` mode, or a single JSON object in
//     `json` mode. Nothing else is written to stdout.
//   - **stderr** carries everything else: progress, notices, hints, warnings,
//     the update nag, and hook output. In `json` mode stdout stays pure JSON
//     so an agent can pipe it straight into a parser.

use serde::Serialize;

/// Output format selected by the global `--format` flag. `Human` (the default)
/// is the aligned/labelled text; `Json` emits a single object on stdout.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, clap::ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
}

impl OutputFormat {
    /// True when machine output is requested — used to suppress decorative
    /// stderr chatter (progress spinners, the update nag) that would clutter
    /// an agent's run even though it never touches stdout.
    pub fn is_json(self) -> bool {
        matches!(self, OutputFormat::Json)
    }
}

/// A command's machine-facing result that also knows how to render itself for
/// humans. Implementors are plain serializable "view" structs; the human
/// rendering lives in `render_human`, so the two representations can't drift
/// out of a single call site (`emit`).
pub trait Render: Serialize {
    /// Render to stdout for a human reader.
    fn render_human(&self);
}

/// Emit a [`Render`] value in the requested format. This is the single place
/// the human/JSON branch lives — commands build a view struct and call `emit`
/// rather than scattering `println!`/`serde_json` across the codebase.
pub fn emit<T: Render>(value: &T, format: OutputFormat) {
    match format {
        OutputFormat::Json => emit_json(value, format),
        OutputFormat::Human => value.render_human(),
    }
}

/// Emit a serializable value as a JSON object on stdout **only in json mode**;
/// a no-op in human mode. For ACTION commands (`new`, `merge`) whose human
/// output is stderr side-effects and whose machine result is an extra stdout
/// object — there's no single "payload" to render for humans, so `Render`
/// (with its `render_human`) doesn't fit; this does.
pub fn emit_json<T: Serialize>(value: &T, format: OutputFormat) {
    if format.is_json() {
        // Serializing owned plain data effectively can't fail; on the off
        // chance it does, surface a diagnostic on stderr and leave stdout
        // empty (valid-empty beats a half-written object).
        match serde_json::to_string_pretty(value) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("error: failed to serialize output: {e}"),
        }
    }
}
