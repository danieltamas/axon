//! Optional integration with RTK (Rust Token Killer) — surfaces its token-savings
//! analytics so the dashboard shows not just what agents *spent* but what was *avoided*.
//!
//! Best-effort and gracefully absent: if `rtk` is not on PATH (or its JSON can't be
//! parsed) we return `None` and the UI/CLI simply omit the panel. Axon never depends on RTK.

use std::process::Command;

use serde::{Deserialize, Serialize};

/// Token-savings summary as surfaced by Axon.
#[derive(Debug, Clone, Serialize)]
pub struct RtkSavings {
    pub commands: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tokens_saved: u64,
    pub saved_pct: f64,
}

#[derive(Deserialize)]
struct Gain {
    summary: GainSummary,
}

#[derive(Deserialize)]
struct GainSummary {
    total_commands: u64,
    total_input: u64,
    total_output: u64,
    total_saved: u64,
    avg_savings_pct: f64,
}

/// Run `rtk gain -f json` and parse its summary. `None` if rtk is missing or errors.
pub fn savings() -> Option<RtkSavings> {
    let out = Command::new("rtk")
        .args(["gain", "-f", "json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let gain: Gain = serde_json::from_slice(&out.stdout).ok()?;
    Some(RtkSavings {
        commands: gain.summary.total_commands,
        input_tokens: gain.summary.total_input,
        output_tokens: gain.summary.total_output,
        tokens_saved: gain.summary.total_saved,
        saved_pct: gain.summary.avg_savings_pct,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rtk_gain_json() {
        let json = r#"{"summary":{"total_commands":37984,"total_input":154481861,
            "total_output":20091322,"total_saved":134504579,"avg_savings_pct":87.068,
            "total_time_ms":1,"avg_time_ms":1}}"#;
        let gain: Gain = serde_json::from_str(json).unwrap();
        assert_eq!(gain.summary.total_commands, 37984);
        assert_eq!(gain.summary.total_saved, 134504579);
        assert!((gain.summary.avg_savings_pct - 87.068).abs() < 1e-6);
    }
}
