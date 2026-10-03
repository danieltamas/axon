//! The ledger verbs as commands (`take`, `done`, `drop`): run in the caller's repository,
//! exit 0 with the outcome line or 1 with the line naming the holder.

use std::process::ExitCode;

use crate::store;

/// Run one ledger command in the caller's repository: exit 0 with its line, or 1 with the
/// line that says who holds the key.
pub fn run(
    db: &std::path::Path,
    run: impl FnOnce(
        &rusqlite::Connection,
        &str,
        &str,
        Option<String>,
    ) -> anyhow::Result<super::Outcome>,
    key: &str,
    note: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let repo = super::repo_of(&std::env::current_dir()?).map_err(crate::Invalid)?;
    let key = super::key(key).map_err(crate::Invalid)?;
    let note = super::note(note).map_err(crate::Invalid)?;
    let mut conn = crate::hub(db)?;
    super::ensure(&conn)?;
    let tx = store::write_tx(&mut conn)?;
    super::expire(&tx, store::now_ms())?;
    let outcome = run(&tx, &repo, &key, note)?;
    tx.commit()?;
    Ok(match outcome {
        super::Outcome::Done(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        super::Outcome::Refused(line) => {
            println!("{line}");
            ExitCode::FAILURE
        }
    })
}
