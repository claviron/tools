# claviron-client

Manage a running Claviron server through its gRPC control plane: inspect its
workers and applications, control services and tasks, and manage certificates,
vhosts, DNS and registrar-held domains. Operations depend on the services
enabled by the target server.

## Install

```sh
cargo install claviron-client --version 0.2.0-alpha.25 --locked --registry crates-io
```

Installation requires a Rust toolchain, native build tools and Protocol Buffers'
`protoc`. Put Cargo's binary directory (normally `~/.cargo/bin`) on `PATH`.

## Connect

Pass `--config PATH` to select `client.yaml`. Without it, the tool checks the
platform configuration directory, `~/.config/claviron/client.yaml`, then
`./client.yaml`. `--server NAME` selects a named profile.

For an mTLS-enabled control plane, a profile contains:

```yaml
profiles:
  example:
    default: true
    grpc:
      server_addr: https://admin.example.test:443
      ca_cert_path: certs/ca.crt
      client_cert_path: certs/client.crt
      client_key_path: certs/client.key
```

Mark exactly one gRPC profile as default. Certificate paths are relative to
the configuration file. Supply credentials trusted by the target server;
keep private keys outside your source repository.

```sh
claviron-client --config ./client.yaml list-profiles
claviron-client --config ./client.yaml --server example info
claviron-client --config ./client.yaml --server example list-applications
```

`list-profiles` reads local configuration without connecting. A full gRPC URL
may be passed to `--server` instead of a profile name; it reuses the default
profile's TLS credentials.

## Commands

`revoke-certificate <CN>` revokes any stored certificate in the server's
hosting database, regardless of issuer, and sets `enabled=false`. The first
revocation date is retained; enabling or renewing the record cannot undo it. Inventory and detail
commands report revocation separately from enabled state. Client-auth snapshots
apply revoked client certificates to new TLS handshakes; disabled server certificates are removed from the SNI resolver.
Existing connections remain open. This local decision does not request
revocation from an external ACME authority.

| Area | Commands |
| --- | --- |
| Server | `info`, `metrics`, `reload`, `shutdown` |
| Workers | `list-workers`, `worker-status`, `start-worker`, `stop-worker`, `reload-worker`, `restart-worker`, `reload-workers`, `restart-workers`, `add-worker`, `remove-worker` |
| Services | `list-services`, `service-status`, `start-service`, `stop-service`, `reload-service`, `restart-service` |
| Embedded applications | `list-applications`, `application-status`, `start-application`, `stop-application`, `restart-application` |
| Link applications | `list-link-applications`, `link-application-status`, `start-link-application`, `stop-link-application`, `restart-link-application`, `send-link-command` |
| Tasks | `list-tasks`, `task-status`, `start-task`, `stop-task`, `reload-task`, `restart-task`, `run-task` |
| Users | `users schema`, `users list`, `users get`, `users create`, `users update`, `users delete` |
| Certificates | `list-certificates`, `list-certificate-inventory`, `get-certificate`, `add-certificate`, `renew-certificate`, `renew-local-certificate`, `create-root-ca`, `create-certificate`, `activate-certificate`, `disable-certificate`, `remove-certificate`, `import-certificate`, `export-certificate`, `reload-resolver` |
| Hosting | `list-hosting`, `list-aliases`, `add-alias`, `remove-alias`, `replace-alias`, `reload-vhosts`, `list-certificates-by-server`, `add-vhost-to-servers`, `remove-vhost-from-servers`, `list-vhosts-by-server`, `list-servers-by-vhost` |
| DNS and domains | `dns`, `domain` subcommands |

Link application commands also accept `list-link`, `link-status`, `start-link`,
`stop-link`, `restart-link` and `send-link`. These commands require a client
and server with the Link RPCs in `ApplicationManager`.

Inspect arguments before running a state-changing operation:

```sh
claviron-client --help
claviron-client restart-application --help
claviron-client run-task --help
claviron-client dns --help
claviron-client domain --help
```

`list-tasks` and `task-status` show each Link task's fleet, kind and declared
worker policy, with its running worker count when verified.

Task start/stop commands wait for the expected fleet placements to report their
state, not just command acceptance. `run-task` waits for the run result, using
its configured execution timeout unless `--timeout` overrides it. Worker
additions remain within the server's configured fleet capacity.

## Metrics

`metrics` sends HTTP `GET /metrics` over the selected server's HTTP/2 connection,
using the same TLS credentials as management commands. It prints Prometheus text
directly to stdout, so the output can be redirected without a JSON wrapper.
The server must expose the endpoint through `claviron-grpc`'s `metrics` feature;
its mTLS and application admission policy still apply. HTTP failures and gRPC
admission refusals return an error, including refusals carried with HTTP 200.

```sh
claviron-client --server example metrics
claviron-client --server example metrics > metrics.prom
```

## Users

The server supplies the user schema, editable forms and management scope.
Commands print JSON, including opaque user references, record revisions and
page cursors. A missing server capability returns its gRPC refusal; the client
never falls back to a database or another identity.

```sh
claviron-client --server example users schema
claviron-client --server example users list --page-size 20 --query operator
claviron-client --server example users get USER_REFERENCE
claviron-client --server example users create --schema-revision SCHEMA_REVISION --data create.json
claviron-client --server example users update USER_REFERENCE --revision RECORD_REVISION --schema-revision SCHEMA_REVISION --data changes.json
claviron-client --server example users delete USER_REFERENCE --revision RECORD_REVISION
```

`list` reads one page; use `--cursor` with the returned `next_cursor` to continue
with the same query. Create/update input files contain one JSON object matching
the server's form. Updates replace only named top-level fields. Validation
failures exit unsuccessfully; revisions are never refreshed or retried
automatically. Keep input files containing credentials outside source control.

## Client certificate identity

Supply a CN or use `--user USERNAME` as its label. Add `--identifier` for a
separate stable identity in the certificate's URI SAN. A UUID is encoded as
`urn:uuid:<uuid>`; an explicit absolute URI may also be supplied.

```sh
claviron-client --server example create-certificate --user operator --kind client --validity-days 30 --identifier 00000000-0000-4000-8000-000000000007
```

CN and `--user` are alternatives; `--user` and `--identifier` require
`--kind client`. The issuing API validates names and URI text. The client checks
that the issuance response confirms the requested URI; an older server that
ignores the field is not reported as success. Such a request may have committed
a certificate: inspect it before retrying.

The application owns URI namespace semantics, user binding and authorization.
Issuance alone grants no application permissions. Existing peer-only certificate
issuance remains available by omitting `--identifier`. Export writes the actual
signed certificate, including its URI identity, and only writes a private key
when `--key-out` is explicitly requested.
