#!/usr/bin/env bash

set -e
set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
BASE_DIR="$(cd "$SCRIPT_DIR/.." && pwd -P)"

cd "${BASE_DIR}" || exit 1

show_help() {
  echo "Usage: $0 [options]"
  echo "Deploy Claviron CLI/tool binaries to a target host."
  echo ""
  echo "Workspace:"
  echo "  claviron-acme    -> package 'claviron-acme-cli'"
  echo "  claviron-cert    -> package 'claviron-cert'"
  echo "  claviron-client  -> package 'claviron-client'"
  echo "  claviron-hosting -> package 'claviron-hosting-cli'"
  echo ""
  echo "Options:"
  echo "  --bin <name>            Deploy one or more specific tool binaries (repeatable)."
  echo "  --all                   Deploy all tool binaries (default if no --bin is provided)."
  echo "  --mode <debug|release>  Build mode (default: release)."
  echo "  --debug                 Shortcut for --mode debug."
  echo "  --release               Shortcut for --mode release."
  echo "  --host <target_host>    Target deployment host (default: current hostname)."
  echo "  --native                Use native CPU optimizations and local-only deployment."
  echo "  --dry-run               Print deployment commands without executing."
  echo "  -h, --help              Print this help message."
  echo ""
  echo "Binaries:"
  echo "  claviron-acme"
  echo "  claviron-cert"
  echo "  claviron-client"
  echo "  claviron-hosting"
}

if [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
  show_help
  exit
fi

if [ -z "$SERVANT_HOME" ]; then
  echo "Error: SERVANT_HOME environment variable is not set." >&2
  exit 1
fi

DEPLOY_SCRIPT="${SERVANT_HOME}/bin/rust-deploy.sh"

if ! [ -x "$DEPLOY_SCRIPT" ]; then
  echo "Error: $DEPLOY_SCRIPT not found or not executable" >&2
  exit 1
fi

HOSTNAME="$(hostname -s)"

BUILD_MODE="release"
TARGET_HOST="${HOSTNAME}"
NATIVE_BUILD="false"
DRY_RUN="false"
BINS=()
DEPLOY_ALL="false"

while [ $# -gt 0 ]; do
  case "$1" in
  --bin)
    shift
    if [ -z "$1" ]; then
      echo "Error: --bin requires a binary name argument." >&2
      exit 1
    fi
    BINS+=("$1")
    shift
    ;;
  --all)
    BINS=()
    DEPLOY_ALL="true"
    shift
    ;;
  --mode)
    shift
    if [ -z "$1" ]; then
      echo "Error: --mode requires debug or release." >&2
      exit 1
    fi
    BUILD_MODE="$1"
    shift
    ;;
  --debug)
    BUILD_MODE="debug"
    shift
    ;;
  --release)
    BUILD_MODE="release"
    shift
    ;;
  --host)
    shift
    if [ -z "$1" ]; then
      echo "Error: --host requires a host argument." >&2
      exit 1
    fi
    TARGET_HOST="$1"
    shift
    ;;
  --native)
    NATIVE_BUILD="true"
    shift
    ;;
  --dry-run)
    DRY_RUN="true"
    shift
    ;;
  -h | --help)
    show_help
    exit
    ;;
  *)
    echo "Error: Unknown option: $1" >&2
    exit 1
    ;;
  esac
done

if [ "${DEPLOY_ALL}" != "true" ] && [ "${#BINS[@]}" -eq 0 ]; then
  DEPLOY_ALL="true"
fi

if [ "$BUILD_MODE" != "debug" ] && [ "$BUILD_MODE" != "release" ]; then
  echo "Error: Invalid build mode. Use 'debug' or 'release'." >&2
  exit 1
fi

if [ "$NATIVE_BUILD" = "true" ]; then
  LOCAL_OK="false"
  case "$TARGET_HOST" in
  "${HOSTNAME}" | localhost | 127.0.0.1 | ::1)
    LOCAL_OK="true"
    ;;
  esac
  if [ "$LOCAL_OK" != "true" ]; then
    echo "Error: --native builds are CPU-specific and cannot be deployed to remote hosts." >&2
    exit 1
  fi
  NATIVE_RUSTFLAGS="-C target-cpu=native ${RUSTFLAGS:-}"
  NATIVE_CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${BASE_DIR}/target-native}"
fi

case "$TARGET_HOST" in
"${HOSTNAME}" | localhost | 127.0.0.1 | ::1) ;;
*)
  if [ "$DRY_RUN" != "true" ]; then
    if ! ping -c 1 "$TARGET_HOST"; then
      echo "Error: Target host '${TARGET_HOST}' is not reachable." >&2
      exit 1
    fi
  fi
  ;;
esac

run_deploy() {
  local package="$1"
  local bin="$2"

  if [ "$DRY_RUN" = "true" ]; then
    if [ "$NATIVE_BUILD" = "true" ]; then
      printf 'RUSTFLAGS=%q CARGO_TARGET_DIR=%q ' "$NATIVE_RUSTFLAGS" "$NATIVE_CARGO_TARGET_DIR"
    fi
    printf 'BUILD_MODE=%q PACKAGE=%q %q %q %q\n' "$BUILD_MODE" "$package" "$DEPLOY_SCRIPT" "$TARGET_HOST" "$bin"
    return 0
  fi

  echo "Deploying ${bin} (package: ${package}) on ${TARGET_HOST} in ($BUILD_MODE) mode"
  if [ "$NATIVE_BUILD" = "true" ]; then
    RUSTFLAGS="$NATIVE_RUSTFLAGS" CARGO_TARGET_DIR="$NATIVE_CARGO_TARGET_DIR" \
      BUILD_MODE="$BUILD_MODE" PACKAGE="$package" "$DEPLOY_SCRIPT" "${TARGET_HOST}" "$bin"
  else
    BUILD_MODE="$BUILD_MODE" PACKAGE="$package" "$DEPLOY_SCRIPT" "${TARGET_HOST}" "$bin"
  fi
}

deploy_one() {
  local bin="$1"

  case "$bin" in
  claviron-acme)
    run_deploy "claviron-acme-cli" "$bin"
    ;;
  claviron-cert)
    run_deploy "claviron-cert" "$bin"
    ;;
  claviron-client)
    run_deploy "claviron-client" "$bin"
    ;;
  claviron-hosting)
    run_deploy "claviron-hosting-cli" "$bin"
    ;;
  claviron)
    echo "Error: claviron server binary lives in claviron-full." >&2
    echo "Use claviron-full/scripts/deploy.sh instead." >&2
    exit 1
    ;;
  *)
    echo "Error: Unknown binary name: $bin" >&2
    echo "Valid tool binaries: claviron-acme, claviron-cert, claviron-client, claviron-hosting" >&2
    exit 1
    ;;
  esac
}

if [ "$DEPLOY_ALL" = "true" ]; then
  BINS=(claviron-client claviron-hosting claviron-acme claviron-cert)
fi

for bin in "${BINS[@]}"; do
  deploy_one "$bin"
  if [ "$DRY_RUN" != "true" ]; then
    echo "✅ ${bin} deployed successfully"
  fi
done
