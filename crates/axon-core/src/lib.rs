//! Axon core — shared by the `axon` dashboard and the `axon-bus` control plane.
//!
//! Pipeline: raw logs -> [`ingest`] (collapse by `message.id`, attribute sub-agents)
//! -> [`normalize`] (ISO->ms, id hash, cost) -> [`model::Event`] -> [`store`] (SQLite).

pub mod ingest;
pub mod model;
pub mod normalize;
pub mod pricing;
pub mod store;
