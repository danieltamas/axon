//! User config (`~/.config/axon/config.toml`) — currently just optional budget caps.
//! Absent or malformed → all-default (no caps); Axon never requires a config file.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Soft spend cap per local day, in display currency (EUR). `None` = no cap.
    pub budget_eur_per_day: Option<f64>,
    /// Soft spend cap per local week (Mon–Sun). `None` = no cap.
    pub budget_eur_per_week: Option<f64>,
    /// Soft spend cap per local calendar month. `None` = no cap.
    pub budget_eur_per_month: Option<f64>,
}

impl Config {
    /// Load the config, returning defaults if the file is missing or unparseable.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_budgets() {
        let c: Config =
            toml::from_str("budget_eur_per_day = 20.0\nbudget_eur_per_week = 100.0").unwrap();
        assert_eq!(c.budget_eur_per_day, Some(20.0));
        assert_eq!(c.budget_eur_per_week, Some(100.0));
    }

    #[test]
    fn empty_is_no_caps() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.budget_eur_per_day.is_none());
    }
}
