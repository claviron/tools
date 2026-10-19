# claviron-tools

This repository contains Claviron operator tools. Each
binary is its own Cargo package. The root `claviron-tools` package is their
shared library; the workspace groups those independently packaged tools.

## Install and use

These are operator commands, not a server. Install only the tools you need
with Cargo; `claviron-tools` itself is a library, not an installable command.

| Package | Command | Purpose |
| --- | --- | --- |
| [claviron-cert](crates/cert/README.md) | `claviron-cert` | Create local CAs and server/client certificates for TLS and mTLS. |
| [claviron-client](crates/client/README.md) | `claviron-client` | Manage a running Claviron server through its gRPC control plane. |
| [claviron-acme-cli](crates/acme/README.md) | `claviron-acme` | Register ACME accounts, issue certificates and renew them using local files. |
| [claviron-hosting-cli](crates/hosting/README.md) | `claviron-hosting` | Manage hosting database records and import/export certificates and ACME accounts. |

Each tool's README covers installation, configuration and commands.

## Boundary

Tools consume core Claviron through the public `claviron` facade and its
features. They must not depend on core-internal crate paths.

Tools may depend on server/app-layer crates from the `claviron-extra`
repository by package dependency. They must not depend on extra-internal
crate paths.

## Shared library

The root library supplies `init_client_logger` and the optional
`claviron_tools::config` module. The binaries remain separate packages under
`crates/`; shared configuration does not need its own package.

| Feature | Shared capability |
| --- | --- |
| None (default) | Standard client logger. |
| `config` | `config::CliConfig` and YAML client configuration loading. |
| `acme` | `config` plus ACME settings. |
| `db` | `config` plus PostgreSQL settings and hosting database profiles. |
| `sqlite` | `db` plus SQLite hosting connections. |

Tools enable only the capabilities they need. Config paths, profile selection,
and file-relative SQLite paths retain the existing client configuration behavior.
