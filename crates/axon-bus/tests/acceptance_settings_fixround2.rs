//! RR-9: distinct service processes and databases, one real owner configuration file.
#![cfg(unix)]
mod common;
use common::fed::api;
use common::*;
use serde_json::json;
use std::sync::Barrier;
use std::time::Duration;

fn amount(config: &toml::Value, key: &str) -> f64 {
    config[key]
        .as_float()
        .or_else(|| config[key].as_integer().map(|n| n as f64))
        .expect("numeric budget")
}

#[test]
fn concurrent_processes_updating_different_budgets_preserve_both_writes() {
    let a = Bus::with_limit(Duration::from_secs(90));
    let b = Bus::with_limit(Duration::from_secs(90));
    a.init();
    b.init();
    let directory = a.root.join("config/axon");
    std::fs::create_dir_all(&directory).unwrap();
    std::os::unix::fs::symlink(&directory, b.root.join("config/axon")).unwrap();
    let path = directory.join("config.toml");
    std::fs::write(
        &path,
        "# keep the owner's note\nowner_flag = 'preserved'\nbudget_eur_per_month = 123.0\n",
    )
    .unwrap();
    let sa = Server::start(&a, true);
    let sb = Server::start(&b, true);
    assert_ne!(sa.process.0.id(), sb.process.0.id());
    assert_ne!(a.db_path(), b.db_path());
    assert_eq!(
        path.canonicalize().unwrap(),
        b.root
            .join("config/axon/config.toml")
            .canonicalize()
            .unwrap()
    );

    // Distinct DBs prevent an unrelated SQLite writer lock from hiding the file race.
    // Fresh values every round detect a lost update even when an older key already exists.
    for round in 1..=40 {
        let daily = f64::from(round);
        let weekly = f64::from(round * 7);
        let barrier = Barrier::new(2);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                api(
                    &a,
                    &sa,
                    "PUT",
                    "/api/settings/budgets",
                    json!({"eur_per_day":daily}),
                );
            });
            let second = scope.spawn(|| {
                barrier.wait();
                api(
                    &b,
                    &sb,
                    "PUT",
                    "/api/settings/budgets",
                    json!({"eur_per_week":weekly}),
                );
            });
            first.join().unwrap();
            second.join().unwrap();
        });
        let stored = std::fs::read_to_string(&path).unwrap();
        let config: toml::Value = toml::from_str(&stored).unwrap();
        assert_eq!(
            amount(&config, "budget_eur_per_day"),
            daily,
            "RR-9: lost daily write in round {round}"
        );
        assert_eq!(
            amount(&config, "budget_eur_per_week"),
            weekly,
            "RR-9: lost weekly write in round {round}"
        );
        assert_eq!(amount(&config, "budget_eur_per_month"), 123.0);
        assert_eq!(config["owner_flag"].as_str(), Some("preserved"));
        assert!(stored.contains("# keep the owner's note"));
    }
    drop(sa);
    drop(sb);
    let restarted = Server::start(&a, true);
    let settings = api(&a, &restarted, "GET", "/api/settings", json!({}));
    for (key, expected) in [
        ("eur_per_day", 40.0),
        ("eur_per_week", 280.0),
        ("eur_per_month", 123.0),
    ] {
        assert_eq!(settings["budgets"][key].as_f64(), Some(expected));
    }
}
