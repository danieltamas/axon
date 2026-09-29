//! axon-bus — the Axon control plane (docs/BUS-PLAN.md). Commands land from M1 on.
//!
//! No async runtime here by design: `axon-bus hook` runs on every tool call and must
//! stay near the process-spawn floor (BUS-PLAN §00, spike Q4).

fn main() {
    println!(
        "axon-bus {} — no commands yet (see docs/BUS-PLAN.md §9, M1)",
        env!("CARGO_PKG_VERSION")
    );
}
