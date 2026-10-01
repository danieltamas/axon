//! Virtual agent (BUS-PLAN §5): one addressable node whose children are a panel of
//! harness:model members and a judge that turns their answers into one.

use crate::registry::{self, Agent, Status};
use crate::store::now_ms;
use crate::Harness;

/// One `harness:model` member, e.g. `codex:gpt-5.5`.
#[derive(Clone, Debug)]
pub struct Member {
    pub harness: Harness,
    pub model: String,
}

pub fn parse_member(text: &str) -> Result<Member, String> {
    let (harness, model) = text
        .split_once(':')
        .filter(|(_, model)| !model.is_empty())
        .ok_or_else(|| format!("invalid member {text}; use harness:model"))?;
    let harness = Harness::ALL
        .into_iter()
        .find(|h| h.as_str() == harness)
        .ok_or_else(|| format!("unknown harness {harness} in {text}"))?;
    Ok(Member {
        harness,
        model: model.to_owned(),
    })
}

/// Register the virtual node under `parent` with its panel and judge as children, all
/// idle; nothing is launched. Returns the node's id. Call inside a write transaction.
pub fn register(
    conn: &rusqlite::Connection,
    parent: &str,
    task: &str,
    panel: &[Member],
    judge: &Member,
) -> anyhow::Result<String> {
    let seed = format!("{parent}\n{task}\n{}\n{}", now_ms(), std::process::id());
    let id = format!("virtual-{}", &blake3::hash(seed.as_bytes()).to_hex()[..12]);
    registry::upsert(
        conn,
        &Agent {
            id: &id,
            harness: "virtual",
            session_id: &id,
            parent_id: Some(parent),
            mission: Some(task),
            ..Default::default()
        },
        Status::Idle,
    )?;
    let judge_id = format!("{id}.judge");
    let panel_ids: Vec<String> = (1..=panel.len())
        .map(|i| format!("{id}.panel-{i}"))
        .collect();
    let members = panel_ids
        .iter()
        .zip(panel)
        .map(|(child, member)| (child, member, "panel"))
        .chain([(&judge_id, judge, "judge")]);
    for (child, member, role) in members {
        registry::upsert(
            conn,
            &Agent {
                id: child,
                harness: member.harness.as_str(),
                session_id: child,
                parent_id: Some(&id),
                model: Some(&member.model),
                role: Some(role),
                mission: Some(task),
                ..Default::default()
            },
            Status::Idle,
        )?;
    }
    Ok(id)
}
