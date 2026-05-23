// ===========================================================================
// terminal/script - Generate the script run inside the spawned tab
// ===========================================================================
//
// Each terminal backend hands off to a fresh shell process; the script
// generated here is what that shell runs. The shape:
//
//   1. Set `WT_SPAWNED_IN_TAB=1` (recursion guard).
//   2. Allocate a temp `--path-file` and call our binary's `wt new` with
//      the given args + the path file.
//   3. If the binary wrote a path to line 1, `cd` to it.
//   4. If snap mode, run the snap-resume loop (eval snap_cmd, then
//      `wt snap-continue`, switch on exit code per the protocol in
//      `src/cli/commands/snap/resume.rs`):
//        - 0 → cd to the recorded "go back" path, break
//        - 2 → reopen (next iteration of the loop)
//        - 3 → preserve worktree, stay put, break
//        - other → break (treat as preserve)
//
// PowerShell and POSIX shell (bash/zsh) are both supported because that's
// what Windows Terminal (pwsh) and iTerm2/GNOME Terminal (sh) hand us.

use super::TabSpec;
use super::SPAWNED_IN_TAB_ENV;

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

/// Build the PowerShell command body for the spawned tab.
pub fn build_pwsh(spec: &TabSpec) -> String {
    let exe = ps_quote(&spec.binary.to_string_lossy());
    let new_args_ps = spec
        .args
        .iter()
        .map(|a| ps_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

    if spec.is_snap {
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
                             if ($reopen -gt 0) {{ Write-Host (\"[wt] Reopen #$reopen\") }} \
                             Write-Host (\"Entering snap mode: $snap\"); \
                             Write-Host (\"Worktree: $(Split-Path $target -Leaf)\"); \
                             Write-Host \"---\"; \
                             Invoke-Expression $snap; \
                             $agentStatus=$LASTEXITCODE; \
                             if ($agentStatus -ne 0) {{ Write-Host (\"[wt] Agent exited with status $agentStatus; checking worktree state...\") }} \
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
    let exe = sh_quote(&spec.binary.to_string_lossy());
    let new_args_sh = spec
        .args
        .iter()
        .map(|a| sh_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

    if spec.is_snap {
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
                         [ $reopen -gt 0 ] && echo \"[wt] Reopen #$reopen\"; \
                         echo \"Entering snap mode: $snap\"; \
                         echo \"Worktree: $(basename \"$target\")\"; \
                         echo \"---\"; \
                         eval \"$snap\"; \
                         agent_status=$?; \
                         [ $agent_status -ne 0 ] && echo \"[wt] Agent exited with status $agent_status; checking worktree state...\"; \
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
            binary: PathBuf::from("/usr/bin/wt"),
            args: args.into_iter().map(String::from).collect(),
            is_snap: snap,
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
        assert!(cmd.contains("$env:WT_SPAWNED_IN_TAB='1'"));
        assert!(cmd.contains("'/usr/bin/wt'"));
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
        assert!(cmd.contains("export WT_SPAWNED_IN_TAB=1"));
        assert!(cmd.contains("'/usr/bin/wt' new 'feat-x'"));
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
    fn args_with_single_quotes_are_escaped() {
        let spec = make_spec(vec!["it's"], false);
        let pwsh = build_pwsh(&spec);
        let posix = build_posix(&spec);
        assert!(pwsh.contains("'it''s'"));
        assert!(posix.contains(r"'it'\''s'"));
    }
}
