//! Wiring the bus into every harness on this machine as part of running Axon, so that
//! installing Axon is the whole setup. A no-op once wired; a harness that cannot be wired
//! is reported and skipped, never fatal to the dashboard.

use crate::install::{current_exe, detected, install, is_wired};

/// Wire each detected harness whose hooks do not run this binary yet (fresh, or left
/// pointing at another build). A development build wires nothing and says why once.
pub fn ensure_hooks() {
    let exe = match current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("  Hooks         not wired: {err:#}");
            return;
        }
    };
    for harness in detected() {
        match is_wired(harness, &exe) {
            Ok(true) => {}
            _ => {
                if let Err(err) = install(harness) {
                    eprintln!("  Hooks         {}: not wired: {err:#}", harness.as_str());
                }
            }
        }
    }
}
