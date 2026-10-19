# claviron-acme-cli

Register ACME accounts, request certificates and renew them using a local
file store. The installed command is `claviron-acme`, not `claviron-acme-cli`.
For remote certificate operations on a running server, use
[claviron-client](../client/README.md).

## Install

```sh
cargo install claviron-acme-cli --version 0.2.0-alpha.18 --locked --registry crates-io
```

Installation requires a Rust toolchain and native build tools. Put Cargo's
binary directory (normally `~/.cargo/bin`) on `PATH`.

## Configuration

Pass `--config PATH` or set `CLAVIRON_CLIENT_CONFIG_PATH` to select
`client.yaml`. Without an explicit path, the tool checks the platform
configuration directory, `~/.config/claviron/client.yaml`, then `./client.yaml`.
Its `acme` section supplies account, certificate and challenge settings.

`--staging` selects the ACME staging environment; production is the default.
Account and certificate files are selected for that mode. Configure challenge
serving before requesting a certificate: this command does not start an HTTP
server for you.

## Commands

| Command | Operation |
| --- | --- |
| `new-account --basename <NAME>` | Generate an account key and register it with the ACME service. |
| `new <HOSTS>...` | Request a certificate; the first hostname is the common name. |
| `renew <CN>` | Renew a saved certificate when it meets the renewal threshold. |

For a staging account named `example`, also select that account in your ACME
configuration before requesting a certificate:

```sh
claviron-acme --config ./client.yaml --staging new-account --basename example
claviron-acme --config ./client.yaml --staging new example.test
claviron-acme --config ./client.yaml --staging renew example.test
```

Replace `example.test` with a real hostname you control when contacting an
ACME service. `renew --before DAYS` overrides the configured renewal threshold;
renewal is one invocation, not a background scheduler.

```sh
claviron-acme --help
claviron-acme new --help
claviron-acme renew --help
```

Use [claviron-hosting](../hosting/README.md) to import these file-based accounts
and certificates into a hosting database.
