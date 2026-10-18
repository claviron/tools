use super::*;

#[test]
fn new_zone_requires_explicit_timers_and_preserves_zero() {
    assert!(new_soa(SoaArgs::default()).is_err());
    let args = SoaArgs {
        mname: Some("ns.example.test".into()),
        rname: Some("hostmaster.example.test".into()),
        refresh: Some(0),
        retry: Some(0),
        expire: Some(0),
        minimum: Some(0),
    };
    assert_eq!(new_soa(args).unwrap().minimum, 0);
}

#[test]
fn dns_commands_parse_without_contacting_a_server() {
    use clap::{CommandFactory, Parser};
    crate::Options::command().debug_assert();
    let options = crate::Options::try_parse_from([
        "claviron-client",
        "dns",
        "add-record",
        "example.test",
        "@",
        "A",
        "192.0.2.1",
        "--ttl",
        "0",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        crate::Command::Dns {
            command: Command::AddRecord {
                config: RecordArgs { ttl: 0, .. },
                ..
            }
        }
    ));
    assert!(
        crate::Options::try_parse_from([
            "claviron-client",
            "dns",
            "add-record",
            "example.test",
            "@",
            "A",
            "192.0.2.1"
        ])
        .is_err()
    );
    assert!(
        crate::Options::try_parse_from([
            "claviron-client",
            "dns",
            "update-domain",
            "example.test",
            "--enable",
            "--disable"
        ])
        .is_err()
    );
    assert!(
        crate::Options::try_parse_from([
            "claviron-client",
            "dns",
            "update-record",
            "example.test",
            "1",
            "--weight",
            "0",
            "--no-weight"
        ])
        .is_err()
    );
    let options = crate::Options::try_parse_from([
        "claviron-client",
        "dns",
        "remove-zone",
        "example.test",
        "--cascade",
    ])
    .unwrap();
    assert!(matches!(
        options.command,
        crate::Command::Dns {
            command: Command::RemoveDomain { cascade: true, .. }
        }
    ));
}

fn soa() -> dns::SoaConfig {
    dns::SoaConfig {
        mname: "ns1.example.com".into(),
        rname: "hostmaster.example.com".into(),
        refresh: 7_200,
        retry: 3_600,
        expire: 1_209_600,
        minimum: 3_600,
    }
}

fn srv() -> dns::RecordConfig {
    dns::RecordConfig {
        name: "_sip._tcp".into(),
        r#type: "SRV".into(),
        value: "sip.example.com".into(),
        ttl: 300,
        priority: Some(10),
        weight: Some(0),
        port: Some(5060),
        enabled: true,
    }
}

#[test]
fn zone_update_keeps_unnamed_soa_fields() {
    let config = dns::ZoneConfig {
        domain: "example.com".into(),
        soa: Some(soa()),
        enabled: true,
    };
    let args = SoaArgs { refresh: Some(600), ..SoaArgs::default() };

    let merged =
        merge_zone(config, None, &args, &EnabledArgs::default()).unwrap();

    let merged_soa = merged.soa.unwrap();
    assert_eq!(merged_soa.refresh, 600);
    assert_eq!(merged_soa.mname, "ns1.example.com");
    assert_eq!(merged_soa.expire, 1_209_600);
    assert!(merged.enabled);
    assert_eq!(merged.domain, "example.com");
}

#[test]
fn zone_update_renames_and_disables() {
    let config = dns::ZoneConfig {
        domain: "example.com".into(),
        soa: Some(soa()),
        enabled: true,
    };
    let enabled = EnabledArgs { enable: false, disable: true };

    let merged = merge_zone(
        config,
        Some("example.org".into()),
        &SoaArgs::default(),
        &enabled,
    )
    .unwrap();

    assert_eq!(merged.domain, "example.org");
    assert!(!merged.enabled);
}

#[test]
fn record_update_keeps_zero_weight_unless_cleared() {
    let kept = merge_record(
        srv(),
        &RecordUpdateArgs { ttl: Some(60), ..RecordUpdateArgs::default() },
    );
    assert_eq!(kept.weight, Some(0));
    assert_eq!(kept.ttl, 60);
    assert_eq!(kept.port, Some(5060));

    let cleared = merge_record(
        srv(),
        &RecordUpdateArgs {
            record_type: Some("cname".into()),
            no_priority: true,
            no_weight: true,
            no_port: true,
            ..RecordUpdateArgs::default()
        },
    );
    assert_eq!(cleared.r#type, "CNAME");
    assert_eq!(
        (cleared.priority, cleared.weight, cleared.port),
        (None, None, None)
    );
}
