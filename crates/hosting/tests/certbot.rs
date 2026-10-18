use clap::Parser;
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::{PrivateKeyParts, PublicKeyParts};

use super::*;

#[test]
fn rsa_jwk_conversion_preserves_private_key() {
    let original =
        rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).unwrap();
    let original =
        rsa::RsaPrivateKey::from_pkcs8_pem(&original.serialize_pem()).unwrap();
    let encode = |v: &rsa::BigUint| URL_SAFE_NO_PAD.encode(v.to_bytes_be());
    let jwk = serde_json::json!({"kty":"RSA", "n":encode(original.n()), "e":encode(original.e()),
        "d":encode(original.d()), "p":encode(&original.primes()[0]), "q":encode(&original.primes()[1])});
    let (pem, public, algorithm) = account_key(&jwk).unwrap();
    assert_eq!(rsa::RsaPrivateKey::from_pkcs8_pem(&pem).unwrap(), original);
    assert_eq!(algorithm, Algorithm::RS256);
    assert!(public.get("d").is_none());
    assert_eq!(public["n"], jwk["n"]);
}

fn ec_jwk() -> Value {
    let key = p256::SecretKey::from_slice(&[7; 32]).unwrap();
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let point = key.public_key().to_encoded_point(false);
    serde_json::json!({"kty":"EC", "crv":"P-256", "d":URL_SAFE_NO_PAD.encode(key.to_bytes()),
        "x":URL_SAFE_NO_PAD.encode(point.x().unwrap()), "y":URL_SAFE_NO_PAD.encode(point.y().unwrap())})
}

#[test]
fn ec_jwk_requires_matching_public_coordinates_and_cli_requires_directory() {
    let mut jwk = ec_jwk();
    assert_eq!(account_key(&jwk).unwrap().2, Algorithm::ES256);
    jwk["x"] = Value::String("wrong".into());
    assert!(account_key(&jwk).is_err());
    assert!(
        crate::Options::try_parse_from(["claviron-hosting", "import-certbot"])
            .is_err()
    );
    assert!(
        crate::Options::try_parse_from([
            "claviron-hosting",
            "import-certbot",
            "--dir",
            "/etc/letsencrypt"
        ])
        .is_ok()
    );
}

#[tokio::test]
async fn batch_import_is_repeatable_and_continues_after_bad_records() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let cfg: AcmeConfig = serde_yaml::from_str("base_dir: /tmp/acme\nchallenges_dir: /tmp/challenges\nproduction_url: https://acme-v02.api.letsencrypt.org/directory\nstaging_url: https://acme-staging-v02.api.letsencrypt.org/directory\norganization: {name: Test, unit: Test, email: test@example.test}\nrenew_before: 30\nalgorithm: ES256\n").unwrap();
    let account_host =
        url::Url::parse(cfg.base_url(&claviron_acme::AcmeMode::Production))
            .unwrap()
            .host_str()
            .unwrap()
            .to_owned();
    let dir = root.join("accounts").join(&account_host).join("directory/good");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("private_key.json"),
        serde_json::to_vec(&ec_jwk()).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("regr.json"), serde_json::to_vec(&serde_json::json!({"uri":format!("https://{account_host}/acme/acct/7"), "body":{}})).unwrap()).unwrap();
    let bad = root.join("accounts").join(&account_host).join("directory/bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("regr.json"), b"invalid").unwrap();
    std::fs::create_dir_all(root.join("live")).unwrap();
    std::fs::create_dir_all(root.join("renewal")).unwrap();
    let valid = root.join("live/valid.test");
    std::fs::create_dir_all(&valid).unwrap();
    let ca_key =
        claviron::ssl::generators::generate_ca_key().unwrap().serialize_pem();
    let ca_cert = claviron::ssl::generators::generate_ca_cert(
        "Test CA",
        ca_key.as_bytes(),
        30,
        None,
        None,
    )
    .unwrap();
    let leaf_key = claviron::ssl::generators::generate_private_key()
        .unwrap()
        .serialize_pem();
    let mut fullchain = claviron::ssl::generators::generate_leaf_cert(
        CertificateKind::Server,
        "valid.test",
        leaf_key.as_bytes(),
        &ca_cert,
        ca_key.as_bytes(),
        2,
        &["alias.test"],
    )
    .unwrap();
    fullchain.extend_from_slice(&ca_cert);
    std::fs::write(valid.join("fullchain.pem"), fullchain).unwrap();
    std::fs::write(valid.join("privkey.pem"), leaf_key).unwrap();
    std::fs::write(
        root.join("renewal/valid.test.conf"),
        format!(
            "[renewalparams]\naccount = good\nserver = {}\n",
            cfg.base_url(&claviron_acme::AcmeMode::Production)
        ),
    )
    .unwrap();
    let expired = root.join("live/expired.test");
    std::fs::create_dir_all(&expired).unwrap();
    let mut params =
        rcgen::CertificateParams::new(vec!["expired.test".into()]).unwrap();
    params.not_before = rcgen::date_time_ymd(2000, 1, 1);
    params.not_after = rcgen::date_time_ymd(2001, 1, 1);
    let key = rcgen::KeyPair::generate().unwrap();
    std::fs::write(
        expired.join("fullchain.pem"),
        params.self_signed(&key).unwrap().pem(),
    )
    .unwrap();
    let broken = root.join("live/broken.test");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("fullchain.pem"), b"bad PEM").unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("fixtures/certbot.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let hosting = HostingStore::sqlite(pool.clone());
    let store = AcmeStore::sqlite(pool.clone());
    import(root, &hosting, &store, "test", &cfg).await.unwrap();
    import(root, &hosting, &store, "test", &cfg).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM hosting_acme_accounts")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM hosting_certificates")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert!(matches!(
        import_certificate(&expired, root, &HashMap::new(), &hosting, "test")
            .await
            .unwrap(),
        Outcome::Skipped(_)
    ));
    assert!(
        import_certificate(&broken, root, &HashMap::new(), &hosting, "test")
            .await
            .is_err()
    );
    std::fs::write(valid.join("privkey.pem"), key.serialize_pem()).unwrap();
    assert!(
        import_certificate(&valid, root, &HashMap::new(), &hosting, "test")
            .await
            .unwrap_err()
            .contains("mismatch")
    );
    let (id, _, _) = import_account(&dir, &store, &cfg).await.unwrap();
    assert!(id > 0);
    std::fs::write(
        dir.join("private_key.json"),
        serde_json::to_vec(&serde_json::json!({"kty":"unsupported"})).unwrap(),
    )
    .unwrap();
    assert!(import_account(&dir, &store, &cfg).await.is_err());
}
