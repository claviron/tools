use std::fs;
use std::path::PathBuf;

use super::{info_lines, join_pem, kind_label, validate_outputs, write_file};
use crate::proto::certificates::{CertificateInfo, CertificateKind};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "claviron-client-certificates-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn chain_files_join_on_their_own_lines() {
    let joined = join_pem(&[
        "-----BEGIN A-----\nx\n-----END A-----".to_owned(),
        "-----BEGIN B-----\ny\n-----END B-----\n\n".to_owned(),
    ]);
    assert_eq!(
        joined,
        "-----BEGIN A-----\nx\n-----END A-----\n-----BEGIN B-----\ny\n-----END B-----\n"
    );
}

#[test]
fn existing_files_are_kept_unless_forced() {
    let dir = scratch("force");
    let path = dir.join("chain.pem");
    fs::write(&path, "old\n").unwrap();

    assert!(write_file(&path, "new", false, false).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "old\n");

    write_file(&path, "new", true, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn equivalent_output_paths_are_rejected_before_export() {
    let dir = scratch("same-path");
    let path = dir.join("chain.pem");
    let same = dir.join(".").join("chain.pem");
    assert!(validate_outputs(Some(&path), Some(&same), true).is_err());
    assert!(!path.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn linked_output_files_are_rejected_even_with_force() {
    use std::os::unix::fs::symlink;

    let dir = scratch("linked-path");
    let target = dir.join("target.pem");
    fs::write(&target, "original").unwrap();
    let link = dir.join("linked.pem");
    symlink(&target, &link).unwrap();
    assert!(validate_outputs(None, Some(&link), true).is_err());
    let hard_link = dir.join("hard-linked.pem");
    fs::hard_link(&target, &hard_link).unwrap();
    assert!(validate_outputs(None, Some(&hard_link), true).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn export_destinations_require_existing_parent_and_explicit_force() {
    let dir = scratch("preflight");
    let path = dir.join("chain.pem");
    assert!(validate_outputs(Some(&path), None, false).is_ok());
    fs::write(&path, "original").unwrap();
    assert!(validate_outputs(Some(&path), None, false).is_err());
    assert!(validate_outputs(Some(&path), None, true).is_ok());
    assert!(
        validate_outputs(Some(&dir.join("missing/chain.pem")), None, false)
            .is_err()
    );
    assert!(validate_outputs(Some(&dir), None, true).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn key_files_are_private_even_when_replacing() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("secret");
    let fresh = dir.join("fresh.key");
    write_file(&fresh, "key", false, true).unwrap();
    assert_eq!(
        fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let replaced = dir.join("replaced.key");
    fs::write(&replaced, "old").unwrap();
    fs::set_permissions(&replaced, fs::Permissions::from_mode(0o644)).unwrap();
    write_file(&replaced, "key", true, true).unwrap();
    assert_eq!(
        fs::metadata(&replaced).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn roots_report_no_issuer_and_unknown_kinds_stay_visible() {
    let root = CertificateInfo {
        id: 1,
        common_name: "Internal Root".to_owned(),
        kind: CertificateKind::Ca as i32,
        enabled: true,
        ..Default::default()
    };
    let lines = info_lines(&root);
    assert!(lines.contains(&"   Kind: ca".to_owned()));
    assert!(lines.contains(&"   Issuer: none (root CA)".to_owned()));
    assert_eq!(kind_label(0), "not reported");
    assert_eq!(kind_label(9), "unknown (9)");
}
