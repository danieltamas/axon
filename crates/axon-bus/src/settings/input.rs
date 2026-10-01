//! Request bodies of the Settings API (P2P-SPEC §2). Each is checked whole before anything
//! is applied, so an invalid body changes nothing and names the first offending field.

use std::ops::RangeInclusive;

use serde_json::{Map, Value};

/// The field a body got wrong, answered as `400 {"error":"invalid","field":…}`.
#[derive(Debug)]
pub struct Invalid(pub String);

fn invalid(field: &str) -> Invalid {
    Invalid(field.to_owned())
}

/// The body as a JSON object whose keys are all in `allowed`; a stray key is the field named.
fn object(body: &[u8], allowed: &[&str]) -> Result<Map<String, Value>, Invalid> {
    let Ok(Value::Object(map)) = serde_json::from_slice(body) else {
        return Err(invalid("body"));
    };
    match map.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(stray) => Err(Invalid(stray.clone())),
        None => Ok(map),
    }
}

fn whole_number(
    map: &Map<String, Value>,
    field: &str,
    range: RangeInclusive<i64>,
) -> Result<i64, Invalid> {
    map.get(field)
        .and_then(Value::as_i64)
        .filter(|n| range.contains(n))
        .ok_or_else(|| invalid(field))
}

pub struct Capture {
    pub enabled: bool,
    pub narrative_days: i64,
}

pub fn capture(body: &[u8]) -> Result<Capture, Invalid> {
    let map = object(body, &["enabled", "narrative_days"])?;
    Ok(Capture {
        enabled: map
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid("enabled"))?,
        narrative_days: whole_number(&map, "narrative_days", 1..=365)?,
    })
}

/// `None` keeps usage forever.
pub fn usage_retention(body: &[u8]) -> Result<Option<i64>, Invalid> {
    let map = object(body, &["retention_days"])?;
    match map.get("retention_days") {
        Some(Value::Null) => Ok(None),
        _ => whole_number(&map, "retention_days", 1..=3650).map(Some),
    }
}

pub const BUDGET_FIELDS: [&str; 3] = ["eur_per_day", "eur_per_week", "eur_per_month"];

/// The budgets a body names: a cap in EUR, or `None` to remove it. Unnamed ones stay.
pub fn budgets(body: &[u8]) -> Result<Vec<(&'static str, Option<f64>)>, Invalid> {
    let map = object(body, &BUDGET_FIELDS)?;
    BUDGET_FIELDS
        .into_iter()
        .filter_map(|field| map.get(field).map(|value| (field, value)))
        .map(|(field, value)| match value {
            Value::Null => Ok((field, None)),
            Value::Number(n) => n
                .as_f64()
                .filter(|eur| *eur >= 0.0)
                .map(|eur| (field, Some(eur)))
                .ok_or_else(|| invalid(field)),
            _ => Err(invalid(field)),
        })
        .collect()
}

pub struct FederationChange {
    pub enabled: Option<bool>,
    pub relay: Option<String>,
}

pub fn federation(body: &[u8]) -> Result<FederationChange, Invalid> {
    let map = object(body, &["enabled", "relay"])?;
    let enabled = match map.get("enabled") {
        None => None,
        Some(value) => Some(value.as_bool().ok_or_else(|| invalid("enabled"))?),
    };
    let relay = match map.get("relay") {
        None => None,
        Some(Value::String(relay)) if relay_is_valid(relay) => Some(relay.clone()),
        Some(_) => return Err(invalid("relay")),
    };
    if enabled.is_none() && relay.is_none() {
        return Err(invalid("body"));
    }
    Ok(FederationChange { enabled, relay })
}

/// `default`, or an `https://` URL: a relay sees only encrypted traffic, but plain HTTP
/// would still let anyone on the path impersonate it.
fn relay_is_valid(relay: &str) -> bool {
    relay == "default" || (relay.starts_with("https://") && relay.parse::<iroh::RelayUrl>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(result: Result<impl Sized, Invalid>) -> String {
        result.err().expect("rejected").0
    }

    #[test]
    fn bounds_are_inclusive_and_name_the_field() {
        assert!(capture(br#"{"enabled":true,"narrative_days":1}"#).is_ok());
        assert!(capture(br#"{"enabled":true,"narrative_days":365}"#).is_ok());
        assert_eq!(
            field(capture(br#"{"enabled":true,"narrative_days":0}"#)),
            "narrative_days"
        );
        assert_eq!(
            field(capture(br#"{"enabled":1,"narrative_days":7}"#)),
            "enabled"
        );
        assert_eq!(field(capture(br#"{"enabled":true}"#)), "narrative_days");
        assert_eq!(
            field(capture(br#"{"enabled":true,"narrative_days":7,"x":1}"#)),
            "x"
        );
        assert_eq!(field(capture(b"nope")), "body");
        assert_eq!(
            usage_retention(br#"{"retention_days":null}"#).unwrap(),
            None
        );
        assert_eq!(
            field(usage_retention(br#"{"retention_days":1.5}"#)),
            "retention_days"
        );
        assert_eq!(field(usage_retention(br#"{}"#)), "retention_days");
    }

    #[test]
    fn budgets_take_only_named_non_negative_numbers() {
        let named = budgets(br#"{"eur_per_month":4.5,"eur_per_day":null}"#).unwrap();
        assert_eq!(named, [("eur_per_day", None), ("eur_per_month", Some(4.5))]);
        assert_eq!(field(budgets(br#"{"eur_per_week":-1}"#)), "eur_per_week");
        assert_eq!(field(budgets(br#"{"eur_per_day":"3"}"#)), "eur_per_day");
    }

    #[test]
    fn relay_is_default_or_https_and_a_body_must_change_something() {
        assert!(federation(br#"{"relay":"https://relay.example"}"#).is_ok());
        assert_eq!(
            field(federation(br#"{"relay":"http://relay.example"}"#)),
            "relay"
        );
        assert_eq!(field(federation(br#"{}"#)), "body");
    }
}
