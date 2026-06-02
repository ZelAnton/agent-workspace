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

/// Version of the `--format json` contract. Every JSON object `ws` emits
/// carries this as a top-level `schema_version` field (injected centrally in
/// [`emit_json`]), so an agent can gate on the shape it expects.
///
/// **Policy:** bump on a BREAKING change to any command's JSON shape — a field
/// removed or renamed, or its meaning/type changed. Purely *additive* fields
/// (a new key alongside the existing ones) do NOT require a bump; agents must
/// ignore unknown keys.
pub const SCHEMA_VERSION: u32 = 1;

/// Wraps any command payload with the top-level `schema_version` tag. `data`
/// must serialize to a JSON object (all our view structs do) — `flatten`
/// merges its keys up alongside `schema_version`.
#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    schema_version: u32,
    #[serde(flatten)]
    data: &'a T,
}

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

/// Human-facing success line on **stderr** (stdout is reserved for the
/// machine/human payload). Consistent `✓ ` prefix across commands; a no-op in
/// JSON mode so an agent's stderr stays uncluttered by decoration. Use for the
/// terminal "done" message of a mutating command.
pub fn success(format: OutputFormat, msg: impl std::fmt::Display) {
    if !format.is_json() {
        eprintln!("✓ {msg}");
    }
}

/// Human-facing progress/notice line on **stderr** (no prefix). Suppressed in
/// JSON mode. Use for intermediate "doing X…" chatter.
pub fn note(format: OutputFormat, msg: impl std::fmt::Display) {
    if !format.is_json() {
        eprintln!("{msg}");
    }
}

/// Emit a serializable value as a JSON object on stdout **only in json mode**;
/// a no-op in human mode. For ACTION commands (`new`, `merge`) whose human
/// output is stderr side-effects and whose machine result is an extra stdout
/// object — there's no single "payload" to render for humans, so `Render`
/// (with its `render_human`) doesn't fit; this does.
pub fn emit_json<T: Serialize>(value: &T, format: OutputFormat) {
    if format.is_json() {
        // Wrap in the versioned envelope so every JSON object carries
        // `schema_version` — the single injection point for the contract.
        let enveloped = Envelope {
            schema_version: SCHEMA_VERSION,
            data: value,
        };
        // Serializing owned plain data effectively can't fail; on the off
        // chance it does, surface a diagnostic on stderr and leave stdout
        // empty (valid-empty beats a half-written object).
        match serde_json::to_string_pretty(&enveloped) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("error: failed to serialize output: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Sample {
        branch: String,
    }

    #[test]
    fn envelope_injects_schema_version() {
        let env = Envelope {
            schema_version: SCHEMA_VERSION,
            data: &Sample {
                branch: "feat".into(),
            },
        };
        let v = serde_json::to_value(&env).unwrap();
        // Versioned tag at the top level...
        assert_eq!(v["schema_version"], SCHEMA_VERSION);
        // ...and the payload's own keys flattened up alongside it.
        assert_eq!(v["branch"], "feat");
    }
}
