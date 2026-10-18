use super::*;

#[test]
fn imported_account_does_not_replace_an_existing_default() {
    assert!(!should_import_account_as_default(true, true));
    assert!(should_import_account_as_default(true, false));
    assert!(!should_import_account_as_default(false, false));
}

#[test]
fn cli_accepts_independent_account_and_certificate_imports() {
    let options = Options::try_parse_from([
        "claviron-hosting",
        "import-acme-account",
        "roma",
        "--mode",
        "production",
    ])
    .unwrap();
    assert!(
        matches!(options.command, Command::ImportAcmeAccount { name, mode: AcmeMode::Production } if name == "roma")
    );
    assert!(
        Options::try_parse_from([
            "claviron-hosting",
            "import-acme-account",
            "roma"
        ])
        .is_err()
    );
    assert!(
        Options::try_parse_from([
            "claviron-hosting",
            "import-acme-account",
            "roma",
            "--mode",
            "invalid"
        ])
        .is_err()
    );
    for command in ["import-acme-certificate", "acme-import"] {
        assert!(matches!(
            Options::try_parse_from([
                "claviron-hosting",
                command,
                "example.com"
            ])
            .unwrap()
            .command,
            Command::AcmeImport { .. }
        ));
    }
}
