# claviron-cert

Create local certificate authorities and issue TLS or mTLS certificates.
This tool writes certificate and key files; it does not contact an ACME service
or require a running Claviron server.

## Install

```sh
cargo install claviron-cert --version 0.2.0-alpha.23 --locked --registry crates-io
```

Installation requires a Rust toolchain and native build tools. Put Cargo's
binary directory (normally `~/.cargo/bin`) on `PATH`.

## Configuration

Select a certificate-store configuration with `--config PATH` or
`CERTSTORE_CONFIG_PATH`. The store defines where CA material and generated
certificates are kept. Keep private keys outside your source repository.

## Commands

| Command | Operation |
| --- | --- |
| `ca <CNAME> --basename <NAME>` | Create or update a root CA; create its key if missing. |
| `intermediate <CNAME> --basename <NAME>` | Issue an intermediate CA. |
| `server --hosts <HOST>` | Issue a server certificate; repeat `--hosts` for additional names. |
| `client <CNAME>` | Issue an mTLS client certificate. |
| `revoke <NAME>` | Revoke a local client/server certificate and update the signed CRL. |

For example, with a configured certificate store and issuer:

```sh
claviron-cert --config ./certstore.yaml server --hosts example.test
claviron-cert --config ./certstore.yaml client example-client
```

`--issuer` selects an intermediate issuer for server/client certificates.
`--days` sets certificate validity; `--user` and `--group` select ownership of
generated server/client key files.

`revoke` finds the named certificate in the configured store and fails if it is
absent or ambiguous. It resolves the signing CA from the certificate's issuer
or `--issuer`, and requires that CA's local signing key. The signed CRL is
written to `crl_path`, or `revocations.crl` under the store directory when
omitted. Previous entries and other issuers' CRLs are retained. Its `nextUpdate`
is the signing CA's certificate expiry; revocation itself has no expiry or undo.
Reload the consuming certificate source to publish the new file. This command
does not modify hosting records; use `claviron-client revoke-certificate <CN>`
for database-backed management.

```sh
claviron-cert --help
claviron-cert ca --help
claviron-cert server --help
```

## Database-backed issuers

The optional `hosting` feature adds PostgreSQL-backed issuer lookup; `sqlite`
also enables SQLite. For example:

```sh
cargo install claviron-cert --version 0.2.0-alpha.23 --locked --registry crates-io --features sqlite
claviron-cert --help
```

These features add `--hosting-config` and `--server` to select a hosting
configuration and database profile. Use
[claviron-hosting](../hosting/README.md) to manage stored certificate material.
