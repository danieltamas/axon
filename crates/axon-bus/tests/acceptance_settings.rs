//! P2P-SPEC §§1–2, 5: Settings API, S1–S5 and A32/A34 API surface.
//! Browser rendering/interaction is not inferred from these HTTP assertions.
mod common;
use common::fed::*;
use common::settings::*;
use common::*;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn setup(content: bool) -> (Bus, Server) {
    let bus = Bus::with_limit(Duration::from_secs(45));
    bus.init();
    let server = Server::start(&bus, content);
    (bus, server)
}
fn settings(bus: &Bus, server: &Server) -> Value {
    api(bus, server, "GET", "/api/settings", Value::Null)
}
fn update(bus: &Bus, server: &Server, section: &str, body: Value) -> Value {
    let written = api(
        bus,
        server,
        "PUT",
        &format!("/api/settings/{section}"),
        body,
    );
    let read = settings(bus, server);
    // Storage sizes can change between requests; every settings section must agree.
    for key in ["capture", "usage", "budgets", "hooks", "federation"] {
        assert_eq!(
            written[key], read[key],
            "full authoritative settings: {key}"
        );
    }
    assert!(written["storage"].is_object());
    written
}

#[test]
fn defaults_match_the_full_settings_contract_and_all_harnesses_are_listed() {
    let (bus, server) = setup(true);
    let value = settings(&bus, &server);
    assert_eq!(
        value["capture"],
        json!({"enabled":true,"forced_off":false,"narrative_days":7})
    );
    assert_eq!(value["usage"], json!({"retention_days":null}));
    assert_eq!(
        value["budgets"],
        json!({"eur_per_day":null,"eur_per_week":null,"eur_per_month":null})
    );
    assert_eq!(
        value["federation"],
        json!({"enabled":false,"relay":"default","node_id":null,"fingerprint":null})
    );
    assert!(value["storage"]["db_bytes"].as_u64().unwrap() > 0);
    assert!(value["storage"]["wal_bytes"].is_u64());
    assert_eq!(value["storage"]["sessions"], 1);
    let hooks = value["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), HARNESSES.len());
    for harness in HARNESSES {
        let entry = hooks.iter().find(|h| h["harness"] == harness).unwrap();
        assert_eq!(entry["installed"], false);
        assert_eq!(
            std::path::Path::new(entry["config_path"].as_str().unwrap()),
            config(&bus, harness)
        );
    }
    assert!(!bus.root.join("data/axon/fed/identity.key").exists());
}

#[test]
fn capture_and_retention_boundaries_persist_through_restart() {
    let (bus, server) = setup(true);
    for days in [1, 365] {
        let value = update(
            &bus,
            &server,
            "capture",
            json!({"enabled":false,"narrative_days":days}),
        );
        assert_eq!(value["capture"]["enabled"], false);
        assert_eq!(value["capture"]["narrative_days"], days);
        assert_eq!(
            text(
                &bus,
                "SELECT value FROM settings WHERE key=?1",
                "narrative_days"
            ),
            days.to_string()
        );
    }
    assert_eq!(
        count(
            &bus,
            "SELECT count(*) FROM settings WHERE key='capture_enabled'"
        ),
        1
    );
    drop(server);
    let restarted = Server::start(&bus, true);
    assert_eq!(
        settings(&bus, &restarted)["capture"],
        json!({"enabled":false,"forced_off":false,"narrative_days":365})
    );
    bus.register("capture-root", "claude", None);
    capture(&bus, "off", now_ms(), "must-not-be-stored");
    assert_eq!(
        count(
            &bus,
            "SELECT count(*) FROM narrative WHERE text='must-not-be-stored'"
        ),
        0
    );
    update(
        &bus,
        &restarted,
        "capture",
        json!({"enabled":true,"narrative_days":365}),
    );
    capture(&bus, "on", now_ms(), "captured-marker");
    assert_eq!(
        count(
            &bus,
            "SELECT count(*) FROM narrative WHERE text='captured-marker'"
        ),
        1
    );
}

#[test]
fn forced_no_content_cannot_be_overridden_by_a_stored_capture_switch() {
    let (bus, server) = setup(false);
    assert_eq!(settings(&bus, &server)["capture"]["forced_off"], true);
    let value = update(
        &bus,
        &server,
        "capture",
        json!({"enabled":true,"narrative_days":7}),
    );
    assert_eq!(value["capture"]["forced_off"], true);
    bus.register("capture-root", "claude", None);
    capture(&bus, "forced", now_ms(), "forced-off-secret");
    assert_eq!(
        count(
            &bus,
            "SELECT count(*) FROM narrative WHERE text='forced-off-secret'"
        ),
        0
    );
}

#[test]
fn invalid_settings_do_not_partially_apply_and_identify_the_field() {
    let (bus, server) = setup(true);
    let cases = [
        (
            "capture",
            json!({"enabled":false,"narrative_days":0}),
            "narrative_days",
        ),
        (
            "capture",
            json!({"enabled":false,"narrative_days":366}),
            "narrative_days",
        ),
        (
            "capture",
            json!({"enabled":"yes","narrative_days":7}),
            "enabled",
        ),
        ("usage", json!({"retention_days":0}), "retention_days"),
        ("usage", json!({"retention_days":3651}), "retention_days"),
        ("usage", json!({"retention_days":1.5}), "retention_days"),
        ("budgets", json!({"eur_per_day":-0.01}), "eur_per_day"),
        ("budgets", json!({"eur_per_week":-1}), "eur_per_week"),
        ("budgets", json!({"eur_per_month":"10"}), "eur_per_month"),
        ("federation", json!({"relay":"http://127.0.0.1:9"}), "relay"),
    ];
    for (section, body, field) in cases {
        let before = settings(&bus, &server)[section].clone();
        let response = request(
            &bus,
            &server,
            "PUT",
            &format!("/api/settings/{section}"),
            body,
        );
        assert_eq!(response.status, 400);
        assert_eq!(
            parse_json(&response.body),
            json!({"error":"invalid","field":field})
        );
        assert_eq!(settings(&bus, &server)[section], before);
    }
}

#[test]
fn usage_retention_accepts_both_limits_and_null_and_survives_restart() {
    let (bus, server) = setup(true);
    for days in [json!(1), json!(3650), Value::Null] {
        assert_eq!(
            update(&bus, &server, "usage", json!({"retention_days":days}))["usage"]
                ["retention_days"],
            days
        );
    }
    update(&bus, &server, "usage", json!({"retention_days":3650}));
    drop(server);
    let restarted = Server::start(&bus, true);
    assert_eq!(settings(&bus, &restarted)["usage"]["retention_days"], 3650);
}

#[test]
fn retention_deletes_only_older_narrative_and_usage_preserving_the_other_window() {
    let (bus, server) = setup(true);
    bus.register("capture-root", "claude", None);
    update(
        &bus,
        &server,
        "capture",
        json!({"enabled":true,"narrative_days":365}),
    );
    let cut = now_ms() - 24 * 60 * 60 * 1000;
    // A minute either side prevents scheduler latency from changing membership.
    capture(&bus, "old", cut - 60_000, "old-narrative");
    capture(&bus, "young", cut + 60_000, "young-narrative");
    assert_eq!(count(&bus, "SELECT count(*) FROM usage"), 2);
    assert_eq!(
        count(
            &bus,
            "SELECT count(*) FROM narrative WHERE text IS NOT NULL"
        ),
        2
    );
    update(
        &bus,
        &server,
        "capture",
        json!({"enabled":true,"narrative_days":1}),
    );
    eventually(&bus, "narrative cut", || {
        count(
            &bus,
            "SELECT count(*) FROM narrative WHERE text='old-narrative'",
        ) == 0
    });
    assert_eq!(
        count(
            &bus,
            "SELECT count(*) FROM narrative WHERE text='young-narrative'"
        ),
        1
    );
    assert_eq!(
        count(&bus, "SELECT count(*) FROM usage"),
        2,
        "null keeps usage forever"
    );
    update(&bus, &server, "usage", json!({"retention_days":1}));
    eventually(&bus, "usage cut", || {
        count(&bus, "SELECT count(*) FROM usage") == 1
    });
    let ts: i64 = db(&bus)
        .query_row("SELECT ts FROM usage", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ts, cut + 60_000);
    assert_eq!(
        count(
            &bus,
            "SELECT count(*) FROM narrative WHERE text='young-narrative'"
        ),
        1
    );
}

#[test]
fn budgets_keep_comments_and_unrelated_keys_and_are_read_by_the_actual_axon_cli() {
    let (bus, server) = setup(true);
    let path = bus.root.join("config/axon/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "# owner's budget note\nowner_flag = 'keep me'\nbudget_eur_per_week = 99.0\n",
    )
    .unwrap();
    let value = update(
        &bus,
        &server,
        "budgets",
        json!({"eur_per_day":0,"eur_per_month":42.5}),
    );
    for (key, expected) in [
        ("eur_per_day", 0.0),
        ("eur_per_week", 99.0),
        ("eur_per_month", 42.5),
    ] {
        assert_eq!(value["budgets"][key].as_f64(), Some(expected));
    }
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(stored.contains("# owner's budget note"));
    let config: toml::Value = toml::from_str(&stored).unwrap();
    assert_eq!(config["owner_flag"].as_str(), Some("keep me"));
    // The orchestrator builds the workspace binaries; no cargo invocation in a test.
    // A sibling executable avoids CARGO_BIN_EXE_axon (not defined in this package).
    let binary = std::path::Path::new(assert_cmd::cargo::cargo_bin!("axon-bus"))
        .with_file_name(format!("axon{}", std::env::consts::EXE_SUFFIX));
    let mut command = std::process::Command::new(binary);
    bus.isolate(&mut command);
    command.args(["--scan-only", "--no-hooks"]);
    let output = assert_cmd::Command::from(command)
        .timeout(bus.remaining())
        .assert()
        .success()
        .get_output()
        .clone();
    let summary = parse_json(&output.stdout);
    assert_eq!(summary["budget_day_eur"].as_f64(), Some(0.0));
    assert_eq!(summary["budget_week_eur"].as_f64(), Some(99.0));
    assert_eq!(summary["budget_month_eur"].as_f64(), Some(42.5));
    update(&bus, &server, "budgets", json!({"eur_per_day":null}));
    drop(server);
    let restarted = Server::start(&bus, true);
    assert_eq!(
        settings(&bus, &restarted)["budgets"],
        json!({"eur_per_day":null,"eur_per_week":99.0,"eur_per_month":42.5})
    );
}

#[test]
fn every_harness_install_and_uninstall_preserves_other_tools_and_matches_doctor() {
    let (bus, server) = setup(true);
    for harness in HARNESSES {
        let original = owner_config(&bus, harness);
        for _ in 0..2 {
            let value = api(
                &bus,
                &server,
                "POST",
                &format!("/api/settings/hooks/{harness}/install"),
                json!({}),
            );
            let entry = value["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|h| h["harness"] == harness)
                .unwrap();
            assert_eq!(entry["installed"], true);
            let stored = std::fs::read_to_string(config(&bus, harness)).unwrap();
            assert!(stored.contains("owner-model"));
            assert!(stored.contains(if ["claude", "codex"].contains(&harness) {
                "owner-hook"
            } else {
                "owner-plugin"
            }));
            let output = bus.cmd().args(["doctor"]).assert().get_output().clone();
            let report = String::from_utf8(output.stdout).unwrap();
            assert!(
                report
                    .lines()
                    .any(|l| l.starts_with("ok") && l.split_whitespace().nth(1) == Some(harness)),
                "{report}"
            );
        }
        let value = api(
            &bus,
            &server,
            "POST",
            &format!("/api/settings/hooks/{harness}/uninstall"),
            json!({}),
        );
        assert_eq!(
            value["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|h| h["harness"] == harness)
                .unwrap()["installed"],
            false
        );
        assert_eq!(
            std::fs::read_to_string(config(&bus, harness)).unwrap(),
            original
        );
    }
}

#[test]
fn compact_with_a_write_lock_returns_busy_within_one_second_without_data_changes() {
    let (bus, server) = setup(true);
    bus.register("sentinel", "claude", None);
    let before = count(&bus, "SELECT count(*) FROM agents");
    // Fault only: acquire a SQLite write reservation without mutating any row.
    // All assertions use fed::db, which opens SQLITE_OPEN_READ_ONLY.
    let lock = rusqlite::Connection::open_with_flags(
        bus.db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    let response = request(
        &bus,
        &server,
        "POST",
        "/api/settings/storage/compact",
        json!({}),
    );
    assert_eq!(response.status, 409);
    assert_eq!(parse_json(&response.body), json!({"error":"busy"}));
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "1s lock timeout plus scheduling margin"
    );
    assert_eq!(count(&bus, "SELECT count(*) FROM agents"), before);
    lock.execute_batch("ROLLBACK").unwrap();
    let result = api(
        &bus,
        &server,
        "POST",
        "/api/settings/storage/compact",
        json!({}),
    );
    assert!(result["storage"]["db_bytes"].is_u64());
    assert_eq!(count(&bus, "SELECT count(*) FROM agents"), before);
    assert_eq!(
        db(&bus)
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn relay_setting_is_confirmed_persisted_and_does_not_enable_federation() {
    let (bus, server) = setup(true);
    let relay = "https://127.0.0.1:9";
    let value = update(&bus, &server, "federation", json!({"relay":relay}));
    assert_eq!(value["federation"]["relay"], relay);
    assert_eq!(value["federation"]["enabled"], false);
    drop(server);
    let restarted = Server::start(&bus, true);
    assert_eq!(settings(&bus, &restarted)["federation"]["relay"], relay);
    update(&bus, &restarted, "federation", json!({"relay":"default"}));
}
