use super::*;

const T0: i64 = 1_800_000_000_000;
const MINUTE_MS: i64 = 60_000;

fn process(pid: i64, harness: &'static str, cwd: &str, started_ms: i64) -> Session {
    Session {
        pid,
        harness,
        cwd: PathBuf::from(cwd),
        rss: 0,
        started_ms,
    }
}

fn transcript(harness: &str, cwd: &str, first_ts: i64, last_ts: i64) -> Ingested {
    Ingested {
        harness: harness.to_owned(),
        projects: HashSet::from([cwd.to_owned()]),
        main: Usage {
            first_ts,
            last_ts,
            ..Usage::default()
        },
        ..Ingested::default()
    }
}

// R6: session identity survives changes in activity order; ambiguity stays unknown.
#[test]
fn assign_keeps_identity_when_latest_turn_order_flips() {
    let a = process(101, "claude", "/repo", T0);
    let b = process(202, "claude", "/repo", T0 + 10 * MINUTE_MS);
    for open in [[&a, &b], [&b, &a]] {
        let mut ingested = HashMap::from([
            (
                "s1".to_owned(),
                transcript("claude", "/repo", T0 + MINUTE_MS, T0 + 20 * MINUTE_MS),
            ),
            (
                "s2".to_owned(),
                transcript("claude", "/repo", T0 + 11 * MINUTE_MS, T0 + 21 * MINUTE_MS),
            ),
        ]);
        let mut matched = HashMap::new();
        let expected = HashMap::from([(a.pid, "s1"), (b.pid, "s2")]);
        assert_eq!(assign(&open, &ingested, &mut matched), expected);

        ingested.get_mut("s1").unwrap().main.last_ts = T0 + 22 * MINUTE_MS;
        assert_eq!(assign(&open, &ingested, &mut matched), expected);
    }
}

#[test]
fn assign_leaves_two_processes_unknown_when_both_predate_both_sessions() {
    let a = process(101, "claude", "/repo", T0);
    let b = process(202, "claude", "/repo", T0 + 10 * MINUTE_MS);
    let ingested = HashMap::from([
        (
            "s1".to_owned(),
            transcript("claude", "/repo", T0 + 11 * MINUTE_MS, T0 + 20 * MINUTE_MS),
        ),
        (
            "s2".to_owned(),
            transcript("claude", "/repo", T0 + 12 * MINUTE_MS, T0 + 21 * MINUTE_MS),
        ),
    ]);
    for open in [[&a, &b], [&b, &a]] {
        let mut matched = HashMap::new();
        assert!(assign(&open, &ingested, &mut matched).is_empty());
    }
}

#[test]
fn assign_keeps_prior_match_when_another_process_makes_ownership_ambiguous() {
    let a = process(101, "claude", "/repo", T0);
    let b = process(202, "claude", "/repo", T0 + 10 * MINUTE_MS);
    let ingested = HashMap::from([(
        "s1".to_owned(),
        transcript("claude", "/repo", T0 + 11 * MINUTE_MS, T0 + 20 * MINUTE_MS),
    )]);
    let mut matched = HashMap::new();
    let expected = HashMap::from([(a.pid, "s1")]);
    assert_eq!(assign(&[&a], &ingested, &mut matched), expected);
    assert_eq!(assign(&[&b, &a], &ingested, &mut matched), expected);
}

#[test]
fn assign_keeps_prior_match_when_another_session_makes_identity_ambiguous() {
    let a = process(101, "claude", "/repo", T0);
    let mut ingested = HashMap::from([(
        "s1".to_owned(),
        transcript("claude", "/repo", T0 + MINUTE_MS, T0 + 20 * MINUTE_MS),
    )]);
    let mut matched = HashMap::new();
    let expected = HashMap::from([(a.pid, "s1")]);
    assert_eq!(assign(&[&a], &ingested, &mut matched), expected);

    ingested.insert(
        "s2".to_owned(),
        transcript("claude", "/repo", T0 + 11 * MINUTE_MS, T0 + 21 * MINUTE_MS),
    );
    assert_eq!(assign(&[&a], &ingested, &mut matched), expected);
}

#[test]
fn assign_leaves_one_process_unknown_when_two_sessions_are_eligible() {
    let a = process(101, "claude", "/repo", T0);
    let ingested = HashMap::from([
        (
            "s1".to_owned(),
            transcript("claude", "/repo", T0 + MINUTE_MS, T0 + 20 * MINUTE_MS),
        ),
        (
            "s2".to_owned(),
            transcript("claude", "/repo", T0 + 11 * MINUTE_MS, T0 + 21 * MINUTE_MS),
        ),
    ]);
    let mut matched = HashMap::new();
    let assigned = assign(&[&a], &ingested, &mut matched);
    assert!(assigned.is_empty(), "ambiguous identity: {assigned:?}");
}

#[test]
fn assign_matches_one_process_to_one_session_in_its_cwd() {
    let a = process(101, "claude", "/repo", T0);
    let ingested = HashMap::from([(
        "s1".to_owned(),
        transcript("claude", "/repo", T0 + MINUTE_MS, T0 + 20 * MINUTE_MS),
    )]);
    let mut matched = HashMap::new();
    assert_eq!(
        assign(&[&a], &ingested, &mut matched),
        HashMap::from([(a.pid, "s1")])
    );
}

#[test]
fn assign_rejects_a_session_in_another_cwd() {
    let a = process(101, "claude", "/repo", T0);
    let ingested = HashMap::from([(
        "s1".to_owned(),
        transcript("claude", "/repo-other", T0 + MINUTE_MS, T0 + 20 * MINUTE_MS),
    )]);
    let mut matched = HashMap::new();
    assert!(assign(&[&a], &ingested, &mut matched).is_empty());
}

#[test]
fn assign_rejects_a_session_from_another_harness() {
    let a = process(101, "claude", "/repo", T0);
    let ingested = HashMap::from([(
        "s1".to_owned(),
        transcript("codex", "/repo", T0 + MINUTE_MS, T0 + 20 * MINUTE_MS),
    )]);
    let mut matched = HashMap::new();
    assert!(assign(&[&a], &ingested, &mut matched).is_empty());
}

#[test]
fn assign_matches_codex_process_by_its_cd_flag() {
    let os_cwd = PathBuf::from("/axon");
    let cwd = crate::memory::codex_cwd(&["codex", "exec", "-C", "/axon-wt-fed"], os_cwd);
    let mut a = process(101, "codex", "/axon", T0);
    a.cwd = cwd;
    let ingested = HashMap::from([(
        "s1".to_owned(),
        transcript("codex", "/axon-wt-fed", T0 + MINUTE_MS, T0 + 20 * MINUTE_MS),
    )]);
    let mut matched = HashMap::new();
    assert_eq!(
        assign(&[&a], &ingested, &mut matched),
        HashMap::from([(a.pid, "s1")])
    );
}

#[test]
fn assign_rejects_a_codex_session_outside_its_cd_flag() {
    let mut a = process(101, "codex", "/axon", T0);
    a.cwd = crate::memory::codex_cwd(&["codex", "exec", "-C", "/axon-wt-fed"], a.cwd.clone());
    let ingested = HashMap::from([(
        "s1".to_owned(),
        transcript("codex", "/axon", T0 + MINUTE_MS, T0 + 20 * MINUTE_MS),
    )]);
    let mut matched = HashMap::new();
    assert!(assign(&[&a], &ingested, &mut matched).is_empty());
}
