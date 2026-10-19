# claviron-client

Manage a running Claviron server through its gRPC control plane: inspect its
workers and applications, control services and tasks, and manage certificates,
vhosts, DNS and registrar-held domains. Operations depend on the services
enabled by the target server.

## Install

```sh
cargo install claviron-client --version 0.2.0-alpha.18 --locked --registry crates-io
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

| Area | Commands |
| --- | --- |
| Server | `info`, `reload`, `shutdown` |
| Workers | `list-workers`, `worker-status`, `start-worker`, `stop-worker`, `reload-worker`, `restart-worker`, `reload-workers`, `restart-workers`, `add-worker`, `remove-worker` |
| Services | `list-services`, `service-status`, `start-service`, `stop-service`, `reload-service`, `restart-service` |
| Embedded applications | `list-applications`, `application-status`, `start-application`, `stop-application`, `restart-application` |
| Orbit-linked applications | `list-linked-applications`, `linked-application-status`, `start-linked-application`, `stop-linked-application`, `restart-linked-application`, `send-linked-command` |
| Tasks | `list-tasks`, `task-status`, `start-task`, `stop-task`, `reload-task`, `restart-task`, `run-task` |
| Certificates | `list-certificates`, `list-certificate-inventory`, `get-certificate`, `add-certificate`, `renew-certificate`, `renew-local-certificate`, `create-root-ca`, `create-certificate`, `activate-certificate`, `disable-certificate`, `remove-certificate`, `import-certificate`, `export-certificate`, `reload-resolver` |
| Hosting | `list-hosting`, `list-aliases`, `add-alias`, `remove-alias`, `replace-alias`, `reload-vhosts`, `list-certificates-by-server`, `add-vhost-to-servers`, `remove-vhost-from-servers`, `list-vhosts-by-server`, `list-servers-by-vhost` |
| DNS and domains | `dns`, `domain` subcommands |

Inspect arguments before running a state-changing operation:

```sh
claviron-client --help
claviron-client restart-application --help
claviron-client run-task --help
claviron-client dns --help
claviron-client domain --help
```

Task start/stop commands wait for the expected fleet placements to report their
state, not just command acceptance. `run-task` waits for the run result, using
its configured execution timeout unless `--timeout` overrides it. Worker
additions remain within the server's configured fleet capacity.
