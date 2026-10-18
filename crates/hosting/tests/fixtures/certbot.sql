CREATE TABLE hosting_acme_issuers (
 id INTEGER PRIMARY KEY, name TEXT NOT NULL, cn TEXT NOT NULL,
 staging_url TEXT NOT NULL, production_url TEXT NOT NULL, is_default INTEGER NOT NULL
);
INSERT INTO hosting_acme_issuers VALUES (1,'LE','LE','https://acme-staging-v02.api.letsencrypt.org/directory','https://acme-v02.api.letsencrypt.org/directory',1);
CREATE TABLE hosting_acme_accounts (
 id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, url TEXT NOT NULL,
 issuer_id INTEGER NOT NULL, private_key TEXT NOT NULL, jwk TEXT NOT NULL,
 algorithm TEXT NOT NULL, email TEXT NOT NULL, organization TEXT NOT NULL,
 created_at TEXT NOT NULL, last_used_at TEXT, is_default INTEGER NOT NULL
);
CREATE TABLE hosting_servers (
 id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
 created_at TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE hosting_certificate_issuers (
 id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, kind TEXT NOT NULL,
 acme_account_id INTEGER UNIQUE REFERENCES hosting_acme_accounts(id), ca_certificate_id INTEGER UNIQUE
);
CREATE TABLE hosting_certificates (
 id INTEGER PRIMARY KEY, kind TEXT NOT NULL, issuer_id INTEGER REFERENCES hosting_certificate_issuers(id),
 cn TEXT NOT NULL UNIQUE, certs_der BLOB NOT NULL, key_der BLOB NOT NULL,
 expires_at TEXT NOT NULL, issued_at TEXT NOT NULL, updated_at TEXT, enabled INTEGER NOT NULL
);
CREATE TABLE hosting_cert_aliases (
 id INTEGER PRIMARY KEY, cert_id INTEGER NOT NULL, hostname TEXT NOT NULL,
 created_at TEXT NOT NULL, UNIQUE(cert_id, hostname)
);
CREATE TABLE hosting_certificate_server (
 certificate_id INTEGER NOT NULL, server_id INTEGER NOT NULL, created_at TEXT NOT NULL,
 PRIMARY KEY(certificate_id, server_id)
);
