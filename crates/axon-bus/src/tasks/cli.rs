//! `axon bus tasks [--repo] [--range <range>] [--json]`: the dashboard's task list.

use serde_json::Value;

/// Print the tasks, newest first: as the API's JSON rows, or one line each.
pub fn run(db: &std::path::Path, range: &str, repo: bool, as_json: bool) -> anyhow::Result<()> {
    let repo = if repo {
        Some(crate::handled::repo_of(&std::env::current_dir()?).map_err(crate::Invalid)?)
    } else {
        None
    };
    // Refuse a missing hub, then bring an older one's schema up to date before reading it.
    drop(crate::hub(db)?);
    let conn = crate::store::init(db)?;
    let tasks = super::list(&conn, range, repo.as_deref(), false)?;
    if as_json {
        println!("{}", Value::Array(tasks));
        return Ok(());
    }
    for task in &tasks {
        let opened =
            chrono::DateTime::from_timestamp_millis(task["opened_at"].as_i64().unwrap_or(0))
                .unwrap_or_default()
                .format("%Y-%m-%d %H:%M");
        let state = if task["closed_at"].is_null() {
            "open"
        } else {
            "closed"
        };
        println!(
            "{opened}  {:<8} {:<6} €{:.2}  {} turns  {}",
            task["kind"].as_str().unwrap_or_default(),
            state,
            task["cost"]["measured"].as_f64().unwrap_or(0.0),
            task["turns"],
            task["name"].as_str().unwrap_or_default(),
        );
    }
    Ok(())
}
