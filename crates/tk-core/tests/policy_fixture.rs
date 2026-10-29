//! Runs `contracts/fixtures/policy/` against tk-core: the normalizer and the
//! matcher, every case, case 21 (confusable folding) included.

use serde::Deserialize;
use tk_core::policy::{evaluate, normalize_tool_name, Action, Call, PolicyFile};

const POLICY: &str = include_str!("../../../contracts/fixtures/policy/policy.json");
const CASES: &str = include_str!("../../../contracts/fixtures/policy/cases.json");

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    id: u32,
    call: Call,
    expect: Expect,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Expect {
    action: Action,
    normalized_tool: String,
    reason_prefix: String,
}

fn load() -> (PolicyFile, Vec<Case>) {
    let policy: PolicyFile = serde_json::from_str(POLICY).expect("policy.json parses");
    let cases: Cases = serde_json::from_str(CASES).expect("cases.json parses");
    (policy, cases.cases)
}

#[test]
fn fixture_has_thirteen_rules_and_fifty_one_cases() {
    let (policy, cases) = load();
    assert_eq!(
        policy.rules.len(),
        13,
        "10 from session 0, 2 added by integration w1, 1 by integration w2"
    );
    assert_eq!(
        cases.len(),
        51,
        "45 to integration w1, 6 from integration w2 (S5)"
    );
}

#[test]
fn every_case_normalizes_to_the_expected_tool() {
    let (policy, cases) = load();
    for case in cases {
        assert_eq!(
            normalize_tool_name(&case.call.tool, &policy.servers),
            case.expect.normalized_tool,
            "case {}",
            case.id
        );
    }
}

#[test]
fn every_case_gets_the_expected_decision() {
    let (policy, cases) = load();
    for case in cases {
        let d = evaluate(&policy, &case.call);
        assert_eq!(
            d.action, case.expect.action,
            "case {}: {}",
            case.id, d.reason
        );
        assert!(
            d.reason.starts_with(&case.expect.reason_prefix),
            "case {}: reason {:?} does not start with {:?}",
            case.id,
            d.reason,
            case.expect.reason_prefix
        );
    }
}
