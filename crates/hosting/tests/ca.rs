use clap::Parser;
use claviron::ssl::generators;

use super::*;

#[test]
fn client_import_resolves_existing_generator_filenames() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("certstore.yaml");
    std::fs::write(&config, "dir: material\n").unwrap();
    let paths = resolve_kind_paths(
        CertificateKind::Client,
        Some("operator"),
        Some(config),
        None,
        None,
        None,
    )
    .unwrap();
    let dir = temp.path().join("material/clients/operator");
    assert_eq!(
        paths,
        (dir.join(CLIENT_CERT_FILENAME), dir.join(CLIENT_KEY_FILENAME))
    );
    let explicit = resolve_kind_paths(
        CertificateKind::Client,
        None,
        None,
        Some(&dir),
        None,
        None,
    )
    .unwrap();
    assert_eq!(explicit, paths);
}

#[test]
fn certstore_is_local_and_resolves_beside_client_config() {
    assert_eq!(
        config_path(Path::new("/etc/claviron/client.yaml"), None),
        Path::new("/etc/claviron/certstore.yaml")
    );
    assert_eq!(
        config_path(Path::new("client.yaml"), None),
        Path::new("certstore.yaml")
    );
    assert_eq!(
        config_path(
            Path::new("/etc/claviron/client.yaml"),
            Some(Path::new("custom.yaml"))
        ),
        Path::new("custom.yaml")
    );
}

#[test]
fn accepts_ca_material_but_rejects_leaf_and_mismatched_key() {
    let key = generators::generate_ca_key().unwrap();
    let pem = key.serialize_pem();
    let cert =
        generators::generate_ca_cert("Roma CA", pem.as_bytes(), 30, None, None)
            .unwrap();
    let ca = plan_ca(&cert, pem.as_bytes(), &[]).unwrap();
    assert_eq!(ca.kind, CertificateKind::Ca);
    assert_eq!(ca.common_name, "Roma CA");
    assert!(ca.aliases.is_empty());
    assert_eq!(ca.issuer_id, None);
    let leaf_key = generators::generate_private_key().unwrap();
    assert!(plan_ca(&cert, leaf_key.serialize_pem().as_bytes(), &[]).is_err());
    let leaf = generators::generate_leaf_cert(
        CertificateKind::Server,
        "server.test",
        leaf_key.serialize_pem().as_bytes(),
        &cert,
        pem.as_bytes(),
        2,
        &[] as &[String],
    )
    .unwrap();
    let issuers = [IssuerCandidate { id: 7, certs_der: ca.certs_der }];
    assert!(
        plan_ca(&leaf, leaf_key.serialize_pem().as_bytes(), &issuers).is_err()
    );
}

#[test]
fn cli_accepts_directory_or_explicit_certificate_and_key_paths() {
    assert!(
        crate::Options::try_parse_from([
            "claviron-hosting",
            "import-ca",
            "--cert",
            "ca.pem",
            "--key",
            "key.pem"
        ])
        .is_ok()
    );
    assert!(
        crate::Options::try_parse_from([
            "claviron-hosting",
            "import-ca",
            "--cert",
            "ca.pem"
        ])
        .is_err()
    );
    let options = crate::Options::try_parse_from([
        "claviron-hosting",
        "import-ca",
        "--dir",
        "certs/ca",
    ])
    .unwrap();
    let crate::Command::ImportCa { dir, cert, key, .. } = options.command
    else {
        panic!("expected CA import");
    };
    let paths =
        input_paths(dir.as_deref(), cert.as_deref(), key.as_deref()).unwrap();
    assert_eq!(
        paths,
        (
            Path::new("certs/ca").join(CA_CERT_FILENAME),
            Path::new("certs/ca").join(CA_KEY_FILENAME)
        )
    );
    let overridden =
        input_paths(dir.as_deref(), Some(Path::new("chain.pem")), None)
            .unwrap();
    assert_eq!(overridden.0, Path::new("chain.pem"));
    assert_eq!(overridden.1, paths.1);
    assert!(
        crate::Options::try_parse_from(["claviron-hosting", "import-ca"])
            .is_err()
    );
}

#[test]
fn named_ca_uses_shared_store_and_explicit_paths_override_it() {
    let options = crate::Options::try_parse_from([
        "claviron-hosting",
        "import-ca",
        "--basename",
        "root",
    ])
    .unwrap();
    assert!(
        matches!(options.command, crate::Command::ImportCa { basename: Some(ref basename), .. } if basename == "root")
    );
    for command in ["import-ca", "import-server", "import-client"] {
        for flag in ["-b", "--basename"] {
            assert!(
                crate::Options::try_parse_from([
                    "claviron-hosting",
                    command,
                    flag,
                    "root"
                ])
                .is_ok()
            );
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("certstore.yaml");
    std::fs::write(&config, "dir: material\n").unwrap();
    let paths = resolve_input_paths(
        Some("root"),
        Some(config.clone()),
        None,
        None,
        None,
    )
    .unwrap();
    let ca_dir = temp.path().join("material").join("ca").join("root");
    assert_eq!(
        paths,
        (ca_dir.join(CA_CERT_FILENAME), ca_dir.join(CA_KEY_FILENAME))
    );
    let overridden = resolve_input_paths(
        Some("root"),
        Some(config.clone()),
        None,
        Some(Path::new("override.pem")),
        None,
    )
    .unwrap();
    assert_eq!(overridden.0, Path::new("override.pem"));
    assert_eq!(overridden.1, paths.1);
    assert!(
        resolve_input_paths(Some("../root"), Some(config), None, None, None)
            .is_err()
    );
    let explicit = resolve_input_paths(
        None,
        Some(temp.path().join("missing.yaml")),
        Some(Path::new("explicit")),
        None,
        None,
    )
    .unwrap();
    assert_eq!(explicit.0, Path::new("explicit").join(CA_CERT_FILENAME));
}
