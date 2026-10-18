use std::path::Path;

use super::ClientConfigFile;

const PROFILES: &str = r#"
profiles:
  local:
    default: true
    grpc:
      server_addr: https://localhost:50051
      ca_cert_path: certs/ca.pem
      client_cert_path: certs/client.pem
      client_key_path: certs/client-key.pem
  remote:
    grpc:
      server_addr: https://remote.example.com:50051
      ca_cert_path: certs/remote-ca.pem
"#;

#[test]
fn hosting_only_profile_does_not_replace_grpc_default() {
    let file: ClientConfigFile = serde_yaml::from_str(&format!("{PROFILES}\n  hosting:\n    default: true\n    hosting: {{ driver: sqlite, path: hosting.db }}\n")).unwrap();
    assert_eq!(
        file.select(None, Path::new("client.yaml")).unwrap().grpc.server_addr,
        "https://localhost:50051"
    );
    assert!(file.select(Some("hosting"), Path::new("client.yaml")).is_err());
}

#[test]
fn default_and_named_server_profiles_use_their_own_connection_settings() {
    let file: ClientConfigFile = serde_yaml::from_str(PROFILES).unwrap();
    let path = Path::new("/tmp/claviron/client.yaml");

    let local = file.select(None, path).unwrap();
    assert_eq!(local.grpc.server_addr, "https://localhost:50051");
    assert_eq!(
        local.grpc.client_key_path.as_deref(),
        Some(Path::new("/tmp/claviron/certs/client-key.pem"))
    );

    let remote = file.select(Some("remote"), path).unwrap();
    assert_eq!(remote.grpc.server_addr, "https://remote.example.com:50051");
    assert_eq!(
        remote.grpc.ca_cert_path.as_deref(),
        Some(Path::new("/tmp/claviron/certs/remote-ca.pem"))
    );
}

#[test]
fn direct_address_reuses_default_profile_credentials() {
    let file: ClientConfigFile = serde_yaml::from_str(PROFILES).unwrap();
    let selected = file
        .select(
            Some("https://other.example.com:50051"),
            Path::new("/tmp/claviron/client.yaml"),
        )
        .unwrap();

    assert_eq!(selected.grpc.server_addr, "https://other.example.com:50051");
    assert_eq!(
        selected.grpc.ca_cert_path.as_deref(),
        Some(Path::new("/tmp/claviron/certs/ca.pem"))
    );
}

#[test]
fn unknown_profile_is_not_silently_replaced_by_default() {
    let file: ClientConfigFile = serde_yaml::from_str(PROFILES).unwrap();
    let error = file
        .select(Some("missing"), Path::new("client.yaml"))
        .err()
        .expect("an unknown profile must fail");
    assert!(error.to_string().contains("unknown server profile"));
}

#[test]
fn legacy_single_server_config_remains_default() {
    let file: ClientConfigFile =
        serde_yaml::from_str("grpc:\n  server_addr: http://localhost:50051\n")
            .unwrap();
    let selected = file.select(None, Path::new("client.yaml")).unwrap();

    assert_eq!(selected.grpc.server_addr, "http://localhost:50051");
}

#[test]
fn profiles_require_exactly_one_explicit_default() {
    let without_default = PROFILES.replace("    default: true\n", "");
    let file: ClientConfigFile =
        serde_yaml::from_str(&without_default).unwrap();
    let error = file.select(None, Path::new("client.yaml")).err().unwrap();
    assert!(error.to_string().contains("no default server profile"));

    let with_two_defaults =
        PROFILES.replace("  remote:\n", "  remote:\n    default: true\n");
    let file: ClientConfigFile =
        serde_yaml::from_str(&with_two_defaults).unwrap();
    let error =
        file.select(Some("local"), Path::new("client.yaml")).err().unwrap();
    assert!(error.to_string().contains("multiple default server profiles"));
}
