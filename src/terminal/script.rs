// ===========================================================================
// terminal/script - Generate the script run inside the spawned tab
// ===========================================================================
//
// Each terminal backend hands off to a fresh shell process; the script
// generated here is what that shell runs. The shape:
//
//   1. Set `WS_SPAWNED_IN_TAB=1` (recursion guard).
//   2. Allocate a temp `--path-file` and call our binary's `ws new` with
//      the given args + the path file.
//   3. If the binary wrote a path to line 1, `cd` to it.
//   4. If snap mode, run the snap-resume loop (eval snap_cmd, then
//      `ws snap-continue`, switch on exit code per the protocol in
//      `src/cli/commands/snap/resume.rs`):
//        - 0 → cd to the recorded "go back" path, break
//        - 2 → reopen (next iteration of the loop)
//        - 3 → preserve worktree, stay put, break
//        - other → break (treat as preserve)
//
// PowerShell and POSIX shell (bash/zsh) are both supported because that's
// what Windows Terminal (pwsh) and iTerm2/GNOME Terminal (sh) hand us.

use super::{TabMode, TabSpec, SPAWNED_IN_TAB_ENV};

/// PowerShell quoting: wrap in single quotes; double internal single
/// quotes. Safe for any string (no `\n`/`\r` escapes needed because
/// single-quoted strings are literal in PowerShell).
fn ps_quote(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

/// POSIX shell quoting (bash/zsh/sh): wrap in single quotes; encode any
/// internal single quote as `'\''` (close, escaped-quote, reopen).
fn sh_quote(s: &str) -> String {
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

/// Escape a string for embedding INSIDE a bash single-quoted `printf`
/// format argument — `printf '<here>'`. Handles:
///   - `\` → `\\` (printf consumes `\\` → `\`; otherwise our literal `\033`
///     escape sequences in the surrounding format would mis-parse)
///   - `%`  → `%%` (printf format specifier neutralisation)
///   - `'`  → `'\''` (close single-quote, escaped literal `'`, reopen)
///
/// Shared by `build_posix_cd` and the iTerm2 / GNOME Terminal wrappers
/// that also embed the title into a `printf` format string.
pub(super) fn escape_for_printf_single_quoted(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    clean
        .replace('\\', "\\\\")
        .replace('%', "%%")
        .replace('\'', r"'\''")
}

/// Escape a string for embedding INSIDE a bash double-quoted argument —
/// e.g. `cd "<here>"`. Neutralises shell-active characters that bash
/// expands inside `"..."`:
///   - `\` (escape char)
///   - `"` (close quote)
///   - `$` (variable expansion / command substitution)
///   - `` ` `` (legacy command substitution)
pub(super) fn escape_for_shell_double_quoted(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

/// Build the PowerShell command body for the spawned tab.
pub fn build_pwsh(spec: &TabSpec) -> String {
    match &spec.mode {
        TabMode::OpenAtCwd => build_pwsh_cd(&spec.title),
        TabMode::WtNew { binary, args, is_snap } => {
            build_pwsh_wt_new(binary, args, *is_snap)
        }
    }
}

/// Minimal pwsh script for `ws cd` mode: set the recursion guard, emit
/// OSC 0 to lock the tab title against shell-prompt overrides, then exit
/// the `-Command` script. The user lands at the shell prompt; cwd was
/// already set by `wt.exe new-tab -d <cwd>` natively.
///
/// **Quoting strategy** (security-sensitive — branch names reach this):
/// the title is embedded in a *single-quoted* PowerShell string and
/// concatenated with `[char]27` (ESC) / `[char]7` (BEL) for the OSC 0
/// terminators. Single quotes are literal in PowerShell — no `$`
/// variable expansion, no backtick escape interpretation — so the only
/// character that can break out is `'` itself, which we double-escape
/// (PS single-quote convention). This shuts the door on injection via
/// hostile branch names containing `` ` ``, `$`, `"`, etc.
fn build_pwsh_cd(title: &str) -> String {
    let title_clean: String = title.chars().filter(|c| !c.is_control()).collect();
    let title_quoted = title_clean.replace('\'', "''");
    format!(
        "$env:{env}='1'; \
         [Console]::Write([char]27 + ']0;' + '{title}' + [char]7)",
        env = SPAWNED_IN_TAB_ENV,
        title = title_quoted,
    )
}

fn build_pwsh_wt_new(binary: &std::path::Path, args: &[String], is_snap: bool) -> String {
    let exe = ps_quote(&binary.to_string_lossy());
    let new_args_ps = args
        .iter()
        .map(|a| ps_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

    if is_snap {
        format!(
            // Lines kept compact: PowerShell -Command treats `\n` as a
            // statement separator, so layout is purely readability.
            "$env:{env}='1'; \
             $pf=[System.IO.Path]::GetTempFileName(); \
             try {{ \
                 & {exe} new {args} --path-file $pf; \
                 if (Test-Path $pf) {{ \
                     $lines=@(Get-Content $pf); \
                     $target=$lines[0]; \
                     $snap=if ($lines.Count -gt 1) {{ $lines[1] }} else {{ '' }}; \
                     if ($target -eq $snap) {{ $snap='' }} \
                     if ($target) {{ Set-Location $target }} \
                     if ($snap) {{ \
                         $reopen=0; \
                         while ($true) {{ \
                             if ($reopen -gt 0) {{ Write-Host (\"[ws] Reopen #$reopen\") }} \
                             Write-Host (\"Entering snap mode: $snap\"); \
                             Write-Host (\"Worktree: $(Split-Path $target -Leaf)\"); \
                             Write-Host \"---\"; \
                             Invoke-Expression $snap; \
                             $agentStatus=$LASTEXITCODE; \
                             if ($agentStatus -ne 0) {{ Write-Host (\"[ws] Agent exited with status $agentStatus; checking worktree state...\") }} \
                             & {exe} snap-continue --path-file $pf; \
                             $code=$LASTEXITCODE; \
                             if ($code -eq 0) {{ \
                                 if (Test-Path $pf) {{ Set-Location (Get-Content $pf); Remove-Item $pf -ErrorAction SilentlyContinue }} \
                                 break \
                             }} elseif ($code -eq 2) {{ Remove-Item $pf -ErrorAction SilentlyContinue; $reopen++ }} \
                             else {{ Remove-Item $pf -ErrorAction SilentlyContinue; break }} \
                         }} \
                     }} \
                 }} \
             }} finally {{ if (Test-Path $pf) {{ Remove-Item $pf -ErrorAction SilentlyContinue }} }}",
            env = SPAWNED_IN_TAB_ENV,
            exe = exe,
            args = new_args_ps,
        )
    } else {
        format!(
            "$env:{env}='1'; \
             $pf=[System.IO.Path]::GetTempFileName(); \
             try {{ \
                 & {exe} new {args} --path-file $pf; \
                 if (Test-Path $pf) {{ \
                     $target=(Get-Content $pf | Select-Object -First 1); \
                     if ($target) {{ Set-Location $target }} \
                 }} \
             }} finally {{ if (Test-Path $pf) {{ Remove-Item $pf -ErrorAction SilentlyContinue }} }}",
            env = SPAWNED_IN_TAB_ENV,
            exe = exe,
            args = new_args_ps,
        )
    }
}

/// Build the POSIX shell command body for the spawned tab (bash/zsh).
pub fn build_posix(spec: &TabSpec) -> String {
    match &spec.mode {
        TabMode::OpenAtCwd => build_posix_cd(&spec.title),
        TabMode::WtNew { binary, args, is_snap } => {
            build_posix_wt_new(binary, args, *is_snap)
        }
    }
}

/// Minimal POSIX script for `ws cd` mode: export the recursion guard,
/// emit OSC 0 to lock the tab title, then exec the user's login shell.
/// The terminal already opened at the right cwd via its native flag.
///
/// **Quoting strategy** (security-sensitive — branch names reach this):
/// the title is embedded in a *single-quoted* `printf` format string.
/// Inside single quotes shell-level expansion is suppressed, so we only
/// worry about: `'` (close quote → use `'\''` to reopen), `\` (printf
/// itself processes escapes — `\033`/`\007` must remain literal in the
/// title), and `%` (printf format specifier — would be interpreted as
/// missing-arg if not doubled). All three are escaped.
fn build_posix_cd(title: &str) -> String {
    let title_escaped = escape_for_printf_single_quoted(title);
    format!(
        "export {env}=1; printf '\\033]0;{title}\\007'; exec \"$SHELL\" -l",
        env = SPAWNED_IN_TAB_ENV,
        title = title_escaped,
    )
}

fn build_posix_wt_new(binary: &std::path::Path, args: &[String], is_snap: bool) -> String {
    let exe = sh_quote(&binary.to_string_lossy());
    let new_args_sh = args
        .iter()
        .map(|a| sh_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

    if is_snap {
        format!(
            "export {env}=1; \
             pf=$(mktemp); \
             {exe} new {args} --path-file \"$pf\"; \
             if [ -f \"$pf\" ]; then \
                 target=$(head -n1 \"$pf\"); \
                 snap=$(tail -n1 \"$pf\"); \
                 [ \"$target\" = \"$snap\" ] && snap=''; \
                 [ -n \"$target\" ] && cd \"$target\"; \
                 if [ -n \"$snap\" ]; then \
                     reopen=0; \
                     while true; do \
                         [ $reopen -gt 0 ] && echo \"[ws] Reopen #$reopen\"; \
                         echo \"Entering snap mode: $snap\"; \
                         echo \"Worktree: $(basename \"$target\")\"; \
                         echo \"---\"; \
                         eval \"$snap\"; \
                         agent_status=$?; \
                         [ $agent_status -ne 0 ] && echo \"[ws] Agent exited with status $agent_status; checking worktree state...\"; \
                         {exe} snap-continue --path-file \"$pf\"; \
                         code=$?; \
                         if [ $code -eq 0 ]; then \
                             [ -f \"$pf\" ] && cd \"$(cat \"$pf\")\"; \
                             rm -f \"$pf\"; break; \
                         elif [ $code -eq 2 ]; then \
                             rm -f \"$pf\"; reopen=$((reopen+1)); \
                         else \
                             rm -f \"$pf\"; break; \
                         fi; \
                     done; \
                 fi; \
                 rm -f \"$pf\"; \
             fi; \
             exec \"$SHELL\" -l",
            env = SPAWNED_IN_TAB_ENV,
            exe = exe,
            args = new_args_sh,
        )
    } else {
        format!(
            "export {env}=1; \
             pf=$(mktemp); \
             {exe} new {args} --path-file \"$pf\"; \
             if [ -f \"$pf\" ]; then \
                 target=$(head -n1 \"$pf\"); \
                 [ -n \"$target\" ] && cd \"$target\"; \
                 rm -f \"$pf\"; \
             fi; \
             exec \"$SHELL\" -l",
            env = SPAWNED_IN_TAB_ENV,
            exe = exe,
            args = new_args_sh,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_spec(args: Vec<&str>, snap: bool) -> TabSpec {
        TabSpec {
            title: "t".into(),
            cwd: PathBuf::from("/tmp"),
            mode: TabMode::WtNew {
                binary: PathBuf::from("/usr/bin/ws"),
                args: args.into_iter().map(String::from).collect(),
                is_snap: snap,
            },
        }
    }

    fn make_cd_spec(title: &str) -> TabSpec {
        TabSpec {
            title: title.into(),
            cwd: PathBuf::from("/tmp/some/worktree"),
            mode: TabMode::OpenAtCwd,
        }
    }

    #[test]
    fn ps_quote_doubles_internal_single_quotes() {
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(ps_quote("plain"), "'plain'");
    }

    #[test]
    fn sh_quote_uses_close_escape_reopen_pattern() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote("plain"), "'plain'");
    }

    #[test]
    fn pwsh_non_snap_invokes_binary_with_args_and_path_file() {
        let spec = make_spec(vec!["feat-x", "--base", "main"], false);
        let cmd = build_pwsh(&spec);
        assert!(cmd.contains("$env:WS_SPAWNED_IN_TAB='1'"));
        assert!(cmd.contains("'/usr/bin/ws'"));
        assert!(cmd.contains("'feat-x'"));
        assert!(cmd.contains("'--base'"));
        assert!(cmd.contains("--path-file $pf"));
        // Non-snap path doesn't include the reopen loop variable.
        assert!(!cmd.contains("$reopen"));
    }

    #[test]
    fn pwsh_snap_includes_reopen_loop() {
        let spec = make_spec(vec!["feat-x", "--snap", "claude"], true);
        let cmd = build_pwsh(&spec);
        assert!(cmd.contains("$reopen"));
        assert!(cmd.contains("snap-continue"));
        assert!(cmd.contains("Invoke-Expression $snap"));
    }

    #[test]
    fn posix_non_snap_exports_env_and_execs_shell() {
        let spec = make_spec(vec!["feat-x"], false);
        let cmd = build_posix(&spec);
        assert!(cmd.contains("export WS_SPAWNED_IN_TAB=1"));
        assert!(cmd.contains("'/usr/bin/ws' new 'feat-x'"));
        // After cd, exec the user's shell so the tab stays open.
        assert!(cmd.ends_with(r#"exec "$SHELL" -l"#));
    }

    #[test]
    fn posix_snap_includes_eval_loop() {
        let spec = make_spec(vec!["feat-x", "--snap", "claude"], true);
        let cmd = build_posix(&spec);
        assert!(cmd.contains("eval \"$snap\""));
        assert!(cmd.contains("snap-continue"));
    }

    #[test]
    fn args_with_spaces_are_quoted() {
        let spec = make_spec(vec!["feat with spaces"], false);
        let pwsh = build_pwsh(&spec);
        let posix = build_posix(&spec);
        assert!(pwsh.contains("'feat with spaces'"));
        assert!(posix.contains("'feat with spaces'"));
    }

    #[test]
    fn pwsh_cd_mode_sets_guard_and_emits_osc() {
        let cmd = build_pwsh(&make_cd_spec("feat-x"));
        assert!(cmd.contains("$env:WS_SPAWNED_IN_TAB='1'"));
        // Title embedded as single-quoted literal between ESC and BEL
        // char codes — defends against $/`/" injection from branch names.
        assert!(cmd.contains("[char]27 + ']0;' + 'feat-x' + [char]7"));
        // No path-file dance, no binary re-exec.
        assert!(!cmd.contains("--path-file"));
        assert!(!cmd.contains("new "));
    }

    #[test]
    fn posix_cd_mode_exports_guard_and_emits_osc() {
        let cmd = build_posix(&make_cd_spec("feat-x"));
        assert!(cmd.contains("export WS_SPAWNED_IN_TAB=1"));
        assert!(cmd.contains(r"\033]0;feat-x\007"));
        // No path-file dance, no binary re-exec.
        assert!(!cmd.contains("--path-file"));
        assert!(cmd.ends_with(r#"exec "$SHELL" -l"#));
    }

    #[test]
    fn cd_mode_strips_control_chars_from_title() {
        let cmd = build_pwsh(&make_cd_spec("feat\x07x"));
        assert!(cmd.contains("'featx'"), "control chars filtered out");
    }

    /// PowerShell injection regression: branch name with `$` and backtick
    /// must NOT cause variable expansion or escape interpretation.
    #[test]
    fn pwsh_cd_title_neutralises_injection_chars() {
        let cmd = build_pwsh(&make_cd_spec("feat-$env:PATH-`evil"));
        // Title embedded verbatim inside single quotes — no expansion.
        assert!(cmd.contains("'feat-$env:PATH-`evil'"));
    }

    /// PowerShell single-quote handling: `'` in title is doubled.
    #[test]
    fn pwsh_cd_title_doubles_internal_single_quotes() {
        let cmd = build_pwsh(&make_cd_spec("it's"));
        assert!(cmd.contains("'it''s'"));
    }

    /// POSIX printf format-specifier neutralisation: `%` doubled.
    #[test]
    fn posix_cd_title_doubles_percent() {
        let cmd = build_posix(&make_cd_spec("100% done"));
        assert!(cmd.contains("100%% done"));
    }

    /// POSIX printf backslash and single-quote escaping.
    #[test]
    fn posix_cd_title_escapes_backslash_and_quote() {
        let cmd = build_posix(&make_cd_spec(r"a\b'c"));
        // \ doubled to \\ (printf consumes one); ' closed-reopened.
        assert!(cmd.contains(r"a\\b'\''c"));
    }

    #[test]
    fn args_with_single_quotes_are_escaped() {
        let spec = make_spec(vec!["it's"], false);
        let pwsh = build_pwsh(&spec);
        let posix = build_posix(&spec);
        assert!(pwsh.contains("'it''s'"));
        assert!(posix.contains(r"'it'\''s'"));
    }
}
