use moneykeeper::contexts::classification::public::RolloutEvidence;
use serde_json::Value;

#[test]
fn rollout_requires_real_resolutions_and_strict_failure_threshold() {
    assert!(!RolloutEvidence::default().qualifies());
    let passing = RolloutEvidence {
        high_confidence_resolved: 200,
        accepted: 190,
        outbound_attempts: 200,
        unsuccessful_attempts: 9,
    };
    assert!(passing.qualifies());
    assert!(
        !RolloutEvidence {
            high_confidence_resolved: 199,
            ..passing
        }
        .qualifies()
    );
    assert!(
        !RolloutEvidence {
            accepted: 189,
            ..passing
        }
        .qualifies()
    );
    assert!(
        !RolloutEvidence {
            unsuccessful_attempts: 10,
            ..passing
        }
        .qualifies()
    );
}

#[test]
fn labeled_evaluation_cases_are_not_claimed_as_measured_predictions() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/classification_eval.json")).unwrap();
    assert_eq!(fixture["fixture_version"], 1);
    assert!(fixture["cases"].as_array().unwrap().len() >= 10);
    assert!(
        fixture["measured_predictions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut ids = std::collections::HashSet::new();
    for case in fixture["cases"].as_array().unwrap() {
        assert!(ids.insert(case["case_id"].as_str().unwrap()));
        assert!(case["amount"].is_string());
        assert!(case["expected_path"].is_string() || case["expected_path"].is_null());
    }
}
