//! Cost engine (DESIGN.md §8). Loads `pricing.toml` (rates per 1,000,000 tokens, keyed by
//! canonical model id) and computes a per-turn cost from the five token buckets.
//!
//! Rules: cache *read* ≪ input ≪ cache *write* — never collapse the buckets. A local model
//! is free (`unpriced = false`). A model id missing from the map is `unpriced = true`: its
//! cost is a floor, surfaced loudly, never a silent €0.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

/// The five token buckets that drive cost, taken from a normalized turn.
#[derive(Debug, Clone, Copy, Default)]
pub struct Buckets {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

/// Per-model rates (per 1,000,000 tokens), in the currency the file is authored in.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ModelRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct LocalSection {
    #[serde(default)]
    free_prefixes: Vec<String>,
}

/// Parsed `pricing.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Pricing {
    #[serde(default)]
    pub priced_as_of: Option<String>,
    #[serde(default)]
    pub display_currency: Option<String>,
    /// Multiply rates by this to convert into the display currency (e.g. USD→EUR).
    #[serde(default)]
    pub fx_to_display: Option<f64>,
    #[serde(default)]
    pub models: HashMap<String, ModelRates>,
    #[serde(default)]
    local: LocalSection,
}

impl Pricing {
    pub fn from_toml_str(s: &str) -> anyhow::Result<Self> {
        toml::from_str(s).context("parse pricing.toml")
    }

    /// Load a user override (e.g. `~/.config/axon/pricing.toml`).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("read pricing file {}", path.display()))?;
        Self::from_toml_str(&s)
    }

    /// The bundled defaults (`assets/pricing.toml`), embedded at compile time.
    pub fn bundled() -> Self {
        Self::from_toml_str(include_str!("../assets/pricing.toml"))
            .expect("bundled assets/pricing.toml must parse")
    }

    /// The USD→display-currency multiplier (1.0 if unset).
    pub fn fx(&self) -> f64 {
        self.fx_to_display.unwrap_or(1.0)
    }

    /// A local/self-hosted model (Ollama, local Qwen): free, but **not** "unpriced".
    pub fn is_local(&self, model: &str) -> bool {
        self.local
            .free_prefixes
            .iter()
            .any(|p| !p.is_empty() && model.contains(p.as_str()))
    }

    /// Compute `(cost, unpriced)` for a canonical model id and its token buckets.
    /// The returned cost is already in the display currency (via `fx_to_display`).
    pub fn cost(&self, model: &str, b: &Buckets) -> (f64, bool) {
        if self.is_local(model) {
            return (0.0, false);
        }
        let Some(r) = self.models.get(model) else {
            return (0.0, true);
        };
        let per_m = |tokens: u64, rate: f64| (tokens as f64) * rate / 1_000_000.0;
        let raw = per_m(b.input, r.input)
            + per_m(b.output, r.output)
            + per_m(b.cache_read, r.cache_read)
            + per_m(b.cache_write_5m, r.cache_write_5m)
            + per_m(b.cache_write_1h, r.cache_write_1h);
        (raw * self.fx_to_display.unwrap_or(1.0), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_pricing_parses() {
        let p = Pricing::bundled();
        assert!(p.models.contains_key("claude-opus-4.8"));
    }

    #[test]
    fn known_model_is_priced() {
        let p = Pricing::bundled();
        let (cost, unpriced) = p.cost(
            "claude-sonnet-4.6",
            &Buckets {
                output: 850,
                ..Default::default()
            },
        );
        // 850 * 15.0/1e6 (output) * 0.92 (fx) = 0.011730 EUR
        assert!(
            (cost - 850.0 * 15.0 / 1_000_000.0 * 0.92).abs() < 1e-9,
            "got {cost}"
        );
        assert!(!unpriced, "a model present in the map is priced");
    }

    #[test]
    fn unknown_model_is_flagged_unpriced() {
        let p = Pricing::bundled();
        let (_, unpriced) = p.cost(
            "brand-new-model-x",
            &Buckets {
                output: 100,
                ..Default::default()
            },
        );
        assert!(unpriced);
    }

    #[test]
    fn rates_apply_per_million_with_fx() {
        let toml = r#"
fx_to_display = 0.5
[models."m"]
input = 2.0
output = 4.0
cache_read = 0.0
cache_write_5m = 0.0
cache_write_1h = 0.0
"#;
        let p = Pricing::from_toml_str(toml).unwrap();
        // (1_000_000*2 + 1_000_000*4)/1e6 = 6.0, *0.5 fx = 3.0
        let (cost, unpriced) = p.cost(
            "m",
            &Buckets {
                input: 1_000_000,
                output: 1_000_000,
                ..Default::default()
            },
        );
        assert!(!unpriced);
        assert!((cost - 3.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn local_prefix_is_free_not_unpriced() {
        let p = Pricing::bundled();
        let (cost, unpriced) = p.cost(
            "ollama/qwen2.5",
            &Buckets {
                output: 999,
                ..Default::default()
            },
        );
        assert_eq!(cost, 0.0);
        assert!(!unpriced);
    }
}
