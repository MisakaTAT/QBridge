//! 用固定版本的 OB11 公开 API 表核对动作列表和必填参数。

use qbridge_ob11::{Call, ParseError, STANDARD_ACTIONS};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

#[derive(Deserialize)]
struct Fixture {
    actions: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    action: String,
    input: Map<String, Value>,
    required: Vec<String>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/ob11-public-api.json")).unwrap()
}

#[test]
fn standard_action_list_matches_the_specification() {
    let expected: BTreeSet<_> = fixture()
        .actions
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    let actual: BTreeSet<_> = STANDARD_ACTIONS
        .iter()
        .map(|name| name.to_string())
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn every_standard_action_parses_or_is_explicitly_unsupported() {
    for entry in fixture().actions {
        match Call::parse(&entry.action, entry.input.clone()) {
            Ok(_) => {
                for field in &entry.required {
                    let mut input = entry.input.clone();
                    input.remove(field);
                    assert!(
                        matches!(
                            Call::parse(&entry.action, input),
                            Err(ParseError::BadParam(_))
                        ),
                        "{} without {field} should be rejected",
                        entry.action
                    );
                }
            }
            Err(ParseError::Unsupported(name)) => assert_eq!(name, entry.action),
            Err(error) => panic!("{} failed to parse: {error}", entry.action),
        }
    }
}
