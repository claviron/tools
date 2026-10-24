use super::*;
use crate::Options;
use clap::Parser;

fn record() -> wire::ManagedRecord {
    wire::ManagedRecord {
        key: Some(wire::RecordKey { resource: wire::Resource::User as i32, reference: "opaque-user-ref".into() }),
        revision: "record-v2".into(),
        data_json: br#"{"username":"operator","custom":"value","id":"18446744073709551616"}"#.to_vec(),
    }
}

#[test]
fn list_requires_an_explicit_positive_page_and_retains_query_and_cursor() {
    for args in [
        vec!["client", "users", "list"],
        vec!["client", "users", "list", "--page-size", "0"],
    ] {
        assert!(Options::try_parse_from(args).is_err());
    }
    let options = Options::try_parse_from([
        "client",
        "users",
        "list",
        "--page-size",
        "12",
        "--query",
        "operator",
        "--cursor",
        "opaque-page",
    ])
    .unwrap();
    let crate::Command::Users {
        command: Command::List { page_size, cursor, query },
    } = options.command
    else {
        panic!("wrong command")
    };
    assert_eq!(page_size, 12);
    assert_eq!(cursor.as_deref(), Some("opaque-page"));
    assert_eq!(query.as_deref(), Some("operator"));
}

#[test]
fn mutations_require_explicit_revisions_and_file_input() {
    for args in [
        vec!["client", "users", "create", "--data", "input.json"],
        vec![
            "client",
            "users",
            "update",
            "ref",
            "--data",
            "input.json",
            "--schema-revision",
            "v1",
        ],
        vec!["client", "users", "delete", "ref"],
    ] {
        assert!(Options::try_parse_from(args).is_err());
    }
    let options = Options::try_parse_from([
        "client",
        "users",
        "update",
        "ref",
        "--revision",
        "r1",
        "--schema-revision",
        "s1",
        "--data",
        "input.json",
    ])
    .unwrap();
    let crate::Command::Users {
        command: Command::Update { reference, revision, schema_revision, data },
    } = options.command
    else {
        panic!("wrong command")
    };
    assert_eq!(
        (reference.as_str(), revision.as_str(), schema_revision.as_str()),
        ("ref", "r1", "s1")
    );
    assert_eq!(data, PathBuf::from("input.json"));
}

#[test]
fn arbitrary_user_fields_and_opaque_references_are_not_reinterpreted() {
    let output = record_json(record()).unwrap();
    assert_eq!(output["reference"], "opaque-user-ref");
    assert_eq!(output["revision"], "record-v2");
    assert_eq!(output["data"]["custom"], "value");
    assert_eq!(output["data"]["id"], "18446744073709551616");
    assert_eq!(
        key("external-reference".into()).unwrap().resource,
        wire::Resource::User as i32
    );
    assert!(key(" ".into()).is_err());
}

#[test]
fn json_objects_keep_exact_numbers_and_reject_non_objects_without_echoing_input()
 {
    let number = "18446744073709551616001";
    let value = object(format!("{{\"precise\":{number}}}").as_bytes()).unwrap();
    assert_eq!(value["precise"].to_string(), number);
    for data in [b"[]".as_slice(), b"null", b"{} {}", b"secret-invalid-json"] {
        let error = object(data).unwrap_err().to_string();
        assert!(!error.contains("secret-invalid-json"));
    }
}

#[test]
fn schema_preserves_provider_forms_and_optional_ui_metadata() {
    let output = schema_json(wire::ManagementSchema {
        resource: wire::Resource::User as i32,
        revision: "schema-v1".into(),
        record_schema_json: br#"{"type":"object"}"#.to_vec(),
        create: Some(management::Form {
            data_schema_json: br#"{"type":"object","required":["type"]}"#
                .to_vec(),
            ui_schema_json: Some(
                br#"{"type":"VerticalLayout","elements":[]}"#.to_vec(),
            ),
        }),
        update: None,
        operations: vec![
            wire::Operation::List as i32,
            wire::Operation::Create as i32,
        ],
    })
    .unwrap();
    assert_eq!(output["create"]["data_schema"]["required"][0], "type");
    assert_eq!(output["create"]["ui_schema"]["type"], "VerticalLayout");
    assert_eq!(output["operations"][0], "OPERATION_LIST");
    assert!(output["update"].is_null());
}

#[test]
fn rejected_or_incomplete_mutations_never_report_success() {
    assert!(
        mutation_json(wire::RecordMutationResult { outcome: None }).is_err()
    );
    let error = mutation_json(wire::RecordMutationResult {
        outcome: Some(wire::record_mutation_result::Outcome::Validation(
            management::ValidationFailure {
                violations: vec![management::FieldViolation {
                    path: "/email".into(),
                    code: "invalid".into(),
                    message: "Invalid email".into(),
                }],
            },
        )),
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("no mutation committed"));
    assert!(error.contains("/email"));
    assert_eq!(
        mutation_json(wire::RecordMutationResult {
            outcome: Some(wire::record_mutation_result::Outcome::Record(
                record()
            ))
        })
        .unwrap()["reference"],
        "opaque-user-ref"
    );
}

#[test]
fn malformed_user_records_are_protocol_errors() {
    let mut invalid = record();
    invalid.key = None;
    assert!(record_json(invalid).is_err());
    let mut invalid = record();
    invalid.revision.clear();
    assert!(record_json(invalid).is_err());
    let mut invalid = record();
    invalid.key.as_mut().unwrap().resource = wire::Resource::Role as i32;
    assert!(record_json(invalid).is_err());
}
