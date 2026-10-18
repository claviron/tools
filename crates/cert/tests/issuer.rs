use clap::Parser;

use super::*;

#[test]
fn file_sources_keep_certificate_and_key_order_and_reject_database_identity() {
    let store = |authority: &str| {
        serde_yaml::from_str::<CertStore>(&format!(
            "dir: certstore\nauthority: {authority}\n"
        ))
        .unwrap()
    };
    assert_eq!(
        resolve_issuer_paths(&store("'Roma CA'"), &None).unwrap(),
        (
            PathBuf::from("certstore/ca/Roma CA/ca.crt"),
            PathBuf::from("certstore/ca/Roma CA/ca.key")
        )
    );
    assert_eq!(
        resolve_issuer_paths(&store("{path: ca/roma/ca.crt}"), &None).unwrap(),
        (
            PathBuf::from("certstore/ca/roma/ca.crt"),
            PathBuf::from("certstore/ca/roma/ca.key")
        )
    );
    assert_eq!(
        resolve_issuer_paths(
            &store("{cert_path: ca.pem, key_path: signing.key}"),
            &None
        )
        .unwrap(),
        (PathBuf::from("ca.pem"), PathBuf::from("signing.key"))
    );
    let db = store("{cn: 'Roma CA'}");
    assert!(
        resolve_issuer_paths(&db, &None)
            .unwrap_err()
            .to_string()
            .contains("file-only")
    );
    assert_eq!(
        resolve_issuer_paths(&db, &Some("local".into())).unwrap(),
        (
            PathBuf::from("certstore/ca/local/ca.crt"),
            PathBuf::from("certstore/ca/local/ca.key")
        )
    );
}

#[test]
fn files_take_precedence_and_partial_pairs_never_fall_back() {
    let dir = tempfile::tempdir().unwrap();
    let store: CertStore = serde_yaml::from_str(&format!(
        "dir: '{}'\nauthority: local\n",
        dir.path().display()
    ))
    .unwrap();
    let options =
        crate::Options::parse_from(["claviron-cert", "client", "test"]);
    std::fs::create_dir_all(store.ca_dir("local")).unwrap();
    std::fs::write(store.ca_cert_path("local"), b"certificate").unwrap();
    assert!(matches!(
        read_issuer_materials(&store, &None, &options),
        Err(SSLError::Io { .. })
    ));
    std::fs::write(store.ca_key_path("local"), b"key").unwrap();
    assert_eq!(
        read_issuer_materials(&store, &None, &options).unwrap(),
        (b"certificate".to_vec(), b"key".to_vec())
    );
}

#[test]
fn explicit_cn_does_not_read_files() {
    let dir = tempfile::tempdir().unwrap();
    let store: CertStore = serde_yaml::from_str(&format!(
        "dir: '{}'\nauthority: {{cn: local}}\n",
        dir.path().display()
    ))
    .unwrap();
    std::fs::create_dir_all(store.ca_dir("local")).unwrap();
    std::fs::write(store.ca_cert_path("local"), b"certificate").unwrap();
    std::fs::write(store.ca_key_path("local"), b"key").unwrap();
    #[cfg(feature = "hosting")]
    let options = crate::Options::parse_from([
        "claviron-cert",
        "--hosting-config",
        dir.path().join("missing.yaml").to_str().unwrap(),
        "client",
        "test",
    ]);
    #[cfg(not(feature = "hosting"))]
    let options =
        crate::Options::parse_from(["claviron-cert", "client", "test"]);
    let error =
        read_issuer_materials(&store, &None, &options).unwrap_err().to_string();
    #[cfg(feature = "hosting")]
    assert!(error.contains("failed to read client config"), "{error}");
    #[cfg(not(feature = "hosting"))]
    assert!(error.contains("hosting feature"), "{error}");
}
