# claviron-tools

This repository contains Claviron operator tools. Each
binary is its own Cargo package. The root `claviron-tools` package is their
shared library; the workspace groups those independently packaged tools.

## Boundary

Tools consume core Claviron through the public `claviron` facade and its
features. They must not depend on core-internal crate paths.

Tools may depend on server/app-layer crates from the `claviron-extra`
repository by package dependency. They must not depend on extra-internal
crate paths.

## Package Shape

Small tools stay small:

- `cert` owns local certificate generation/inspection.
- `client` owns gRPC/pubsub control-plane calls. It keeps its
  proto files, generated client stubs, and TLS channel builder locally so
  the publishable client does not depend on the management server crate.
  ACME, alias, and task command handlers stay in domain-specific modules;
  transport-neutral output helpers stay in a shared module. Task start/stop
  waits for every expected fleet member to publish the requested lifecycle,
  with a client-side timeout rather than implying that command acceptance is
  completion. `run-task` first reads the configured execution timeout when no
  override was supplied, sends an RPC deadline longer than that budget, and
  then waits for the synchronous typed run result.
- `acme-cli` owns file-mode ACME account/cert operations.
- `hosting` owns DB-backed hosting/cert import/export. It opens
  a small SQLx postgres pool, performs hosting operations through
  `claviron::hosting::HostingStore`, and performs ACME account operations
  through `claviron_acme::AcmeStore`. SeaORM entities and raw schema queries
  do not cross into the CLI.
- `journal` owns Linux journald display.

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
