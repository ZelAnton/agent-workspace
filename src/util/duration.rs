// ===========================================================================
// util/duration - Human-friendly Duration formatting
// ===========================================================================
//
// Two flavours used across the CLI:
//
//   [`format_step`]   — sub-second-aware, for per-step "took X" messages.
//                       Examples:  "120ms", "0.8s", "3.4s", "1m 12s"
//
//   [`format_total`]  — minutes/hours scale, for aggregate wall time.
//                       Examples:  "5s", "1m 35s", "1h 22m 0s"
//
// Both pick the shortest unit set that captures the magnitude — no
// extraneous "0h 0m " prefixes on a 3-second operation.

use std::time::Duration;

/// Per-operation timing. Sub-second precision when the step is short,
/// rounds to integer seconds (with a "Nm Ms" carry) once we're past a
/// minute. Designed to read naturally as a trailing parenthetical:
///
///   eprintln!("  Stashed uncommitted changes ({}).", format_step(t.elapsed()));
pub fn format_step(d: Duration) -> String {
    let total_ms = d.as_millis();
    if total_ms < 1_000 {
        // Sub-second: just milliseconds. "234ms" reads better than
        // "0.23s" for genuinely fast operations.
        return format!("{total_ms}ms");
    }
    let total_secs = d.as_secs_f64();
    if total_secs < 10.0 {
        // 1-10s window: one decimal place. Catches "this was fast but
        // not instant" cases like "1.2s".
        return format!("{total_secs:.1}s");
    }
    if total_secs < 60.0 {
        // 10-60s: drop the decimal — precision past whole seconds is
        // noise at this scale.
        return format!("{:.0}s", total_secs);
    }
    // Past a minute: split into Nm Ms.
    let total = d.as_secs();
    let m = total / 60;
    let s = total % 60;
    format!("{m}m {s}s")
}

/// Aggregate wall time. Always picks the shortest unit set ("5s",
/// "1m 35s", "1h 22m 0s"). Designed for the final "Elapsed:" line that
/// closes out `ws new`.
pub fn format_total(d: Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_sub_second_is_millis() {
        assert_eq!(format_step(Duration::from_millis(234)), "234ms");
        assert_eq!(format_step(Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn step_one_to_ten_secs_keeps_decimal() {
        assert_eq!(format_step(Duration::from_millis(1_200)), "1.2s");
        assert_eq!(format_step(Duration::from_millis(9_800)), "9.8s");
    }

    #[test]
    fn step_ten_to_sixty_secs_drops_decimal() {
        assert_eq!(format_step(Duration::from_millis(12_500)), "12s");
        assert_eq!(format_step(Duration::from_millis(45_900)), "46s");
    }

    #[test]
    fn step_past_a_minute_uses_m_s() {
        assert_eq!(format_step(Duration::from_secs(75)), "1m 15s");
        assert_eq!(format_step(Duration::from_secs(125)), "2m 5s");
    }

    #[test]
    fn total_seconds_only_for_short() {
        assert_eq!(format_total(Duration::from_secs(5)), "5s");
        assert_eq!(format_total(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn total_minutes_seconds() {
        assert_eq!(format_total(Duration::from_secs(95)), "1m 35s");
        assert_eq!(format_total(Duration::from_secs(125)), "2m 5s");
    }

    #[test]
    fn total_hours_minutes_seconds() {
        assert_eq!(format_total(Duration::from_secs(4920)), "1h 22m 0s");
        assert_eq!(format_total(Duration::from_secs(3725)), "1h 2m 5s");
    }
}
