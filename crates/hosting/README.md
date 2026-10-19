# claviron-hosting-cli

Manage Claviron hosting records and certificate material directly in a
PostgreSQL or SQLite database. The installed command is `claviron-hosting`.
It does not need a running Claviron server or its gRPC control plane.

## Install

```sh
cargo install claviron-hosting-cli --version 0.2.0-alpha.18 --locked --registry crates-io
```

Installation requires a Rust toolchain and native build tools. Put Cargo's
binary directory (normally `~/.cargo/bin`) on `PATH`.

## Configuration

Pass `--config PATH` or set `CLAVIRON_CLIENT_CONFIG_PATH` to select
`client.yaml`. Without an explicit path, the tool checks the platform
configuration directory, `~/.config/claviron/client.yaml`, then `./client.yaml`.
Select a database through `--server PROFILE` and its `profiles.<name>.hosting`
settings. Relative SQLite paths are resolved from the configuration file.
The configuration also contains the `acme` settings used for file imports and
exports.

`--server-name NAME` selects the hosting server identity used by registration
and server-certificate imports; it is distinct from the database profile.
Use an existing hosting schema, then check the selected connection:

```sh
claviron-hosting --config ./client.yaml --server example check
```

`check` verifies the connection and issuer schema without writing records.

## Commands

| Command | Operation |
| --- | --- |
| `check` | Check the selected connection and issuer schema. |
| `register-server` | Register the selected server identity. |
| `import-certbot --dir <PATH>` | Import Certbot accounts and current live certificates. |
| `import-ca` | Import a local CA and signing key. |
| `import-server` | Import a local server certificate and key with a server assignment. |
| `import-client` | Import a local client certificate and key without a server assignment. |
| `import-acme-account <NAME> --mode <MODE>` | Import an account from the configured ACME file store. |
| `import-acme-certificate <CN>` | Import a file-based ACME certificate. |
| `export-certificate <CN>` | Export a certificate to a file-based ACME store. |

Local CA/server/client imports accept a certstore `--basename`, a `--dir`, or
explicit `--cert` and `--key` paths. `--certstore-config PATH` selects the store
used for named imports. Exports accept `--dir`, `--overwrite`, and
`--full` to include matching ACME account metadata. Treat exported private keys
and account credentials as secrets.

```sh
claviron-hosting --help
claviron-hosting import-server --help
claviron-hosting import-acme-account --help
claviron-hosting export-certificate --help
```
