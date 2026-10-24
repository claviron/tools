use super::{Kind, kind_label};
use crate::proto::mtls::CertificateKind;

#[test]
fn operator_kinds_map_to_protocol_kinds() {
    assert_eq!(CertificateKind::from(Kind::Server), CertificateKind::Server);
    assert_eq!(CertificateKind::from(Kind::Client), CertificateKind::Client);
    assert_eq!(CertificateKind::from(Kind::Ca), CertificateKind::Ca);
}

#[test]
fn unknown_kinds_stay_visible() {
    assert_eq!(kind_label(CertificateKind::Client as i32), "client");
    assert_eq!(kind_label(0), "not reported");
    assert_eq!(kind_label(42), "unknown (42)");
}

#[test]
fn certificate_cli_selects_cn_or_user_exclusively() {
    use crate::{Command, Options};
    use clap::Parser;

    for (args, expected) in [
        (
            vec![
                "client",
                "create-certificate",
                "example.test",
                "--kind",
                "server",
                "--validity-days",
                "30",
            ],
            "example.test",
        ),
        (
            vec![
                "client",
                "create-certificate",
                "--user",
                "operator",
                "--kind",
                "client",
                "--validity-days",
                "30",
            ],
            "operator",
        ),
    ] {
        let options = Options::try_parse_from(args).unwrap();
        let Command::CreateCertificate { identity, kind, .. } = options.command
        else {
            panic!("wrong command")
        };
        assert_eq!(identity.common_name(kind).unwrap(), expected);
    }
    assert!(
        Options::try_parse_from([
            "client",
            "create-certificate",
            "name",
            "--user",
            "operator",
            "--kind",
            "client",
            "--validity-days",
            "30"
        ])
        .is_err()
    );
    assert!(
        Options::try_parse_from([
            "client",
            "create-certificate",
            "--kind",
            "client",
            "--validity-days",
            "30"
        ])
        .is_err()
    );
}

#[test]
fn username_option_never_becomes_a_server_or_ca_identity() {
    use super::Identity;
    for kind in [Kind::Server, Kind::Ca] {
        let identity =
            Identity { common_name: None, user: Some("operator".into()) };
        assert!(identity.common_name(kind).is_err());
    }
    for user in [
        "",
        " operator",
        "operator ",
        "operator\n",
        "operator.",
        "*.example.test",
        "operator_name",
    ] {
        let identity = Identity { common_name: None, user: Some(user.into()) };
        assert!(identity.common_name(Kind::Client).is_err());
    }
}
