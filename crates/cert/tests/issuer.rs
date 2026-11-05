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
        resolve_issuer_paths(&store("{local: roma}"), None).unwrap(),
        (
            PathBuf::from("certstore/ca/roma/ca.crt"),
            PathBuf::from("certstore/ca/roma/ca.key")
        )
    );
    assert_eq!(
        resolve_issuer_paths(&store("{path: ca/roma/ca.crt}"), None).unwrap(),
        (
            PathBuf::from("certstore/ca/roma/ca.crt"),
            PathBuf::from("certstore/ca/roma/ca.key")
        )
    );
    assert_eq!(
        resolve_issuer_paths(
            &store("{cert_path: ca.pem, key_path: signing.key}"),
            None
        )
        .unwrap(),
        (PathBuf::from("ca.pem"), PathBuf::from("signing.key"))
    );
    let db = store("{cn: 'Roma CA'}");
    assert!(
        resolve_issuer_paths(&db, None)
            .unwrap_err()
            .to_string()
            .contains("file-only")
    );
    assert_eq!(
        resolve_issuer_paths(&db, Some("local".parse().unwrap())).unwrap(),
        (
            PathBuf::from("certstore/ca/local/ca.crt"),
            PathBuf::from("certstore/ca/local/ca.key")
        )
    );
}

#[test]
fn files_take_precedence_and_partial_pairs_never_fall_back() {
    use claviron::ssl::{formatters::certificate_chain_to_pem, generators};

    let dir = tempfile::tempdir().unwrap();
    let store: CertStore = serde_yaml::from_str(&format!(
        "dir: '{}'\nauthority: local\n",
        dir.path().display()
    ))
    .unwrap();
    let options =
        crate::Options::parse_from(["claviron-cert", "client", "test"]);
    let local = "local".parse().unwrap();
    std::fs::create_dir_all(store.ca_dir(local)).unwrap();
    let key = generators::generate_ca_key().unwrap();
    let cert = generators::generate_ca_cert(
        &"Example CA".parse().unwrap(),
        &key,
        30,
        &Default::default(),
    )
    .unwrap();
    std::fs::write(
        store.ca_cert_path(local),
        certificate_chain_to_pem(cert.as_ref()).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        read_issuer_materials(&store, None, &options),
        Err(SSLError::Io { .. })
    ));
    std::fs::write(store.ca_key_path(local), key.serialize_pem()).unwrap();
    let material = read_issuer_materials(&store, None, &options).unwrap();
    assert_eq!(material.certificates, cert.as_ref());
    assert!(material.key.serialized_der() == key.serialized_der());
}

#[test]
fn explicit_cn_does_not_read_files() {
    let dir = tempfile::tempdir().unwrap();
    let store: CertStore = serde_yaml::from_str(&format!(
        "dir: '{}'\nauthority: {{cn: local}}\n",
        dir.path().display()
    ))
    .unwrap();
    let local = "local".parse().unwrap();
    std::fs::create_dir_all(store.ca_dir(local)).unwrap();
    std::fs::write(store.ca_cert_path(local), b"certificate").unwrap();
    std::fs::write(store.ca_key_path(local), b"key").unwrap();
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
    let error = read_issuer_materials(&store, None, &options)
        .err()
        .unwrap()
        .to_string();
    #[cfg(feature = "hosting")]
    assert!(error.contains("failed to read client config"), "{error}");
    #[cfg(not(feature = "hosting"))]
    assert!(error.contains("hosting feature"), "{error}");
}

#[test]
fn cli_distinguishes_common_names_from_store_basenames() {
    let args = ["claviron-cert", "ca", "Example CA", "--basename"];
    assert!(
        crate::Options::try_parse_from(args.into_iter().chain(["example"]))
            .is_ok()
    );
    for basename in ["../root", "Example CA", "example.test", "longerthan"] {
        assert!(
            crate::Options::try_parse_from(args.into_iter().chain([basename]))
                .is_err()
        );
    }
    assert!(
        crate::Options::try_parse_from(["claviron-cert", "client", " "])
            .is_err()
    );
}
