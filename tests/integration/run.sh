#!/usr/bin/env bash
# Build the NSClient++ image, start it, run the integration suite against it
# and tear it down again.
#
#   tests/integration/run.sh                 # version from .nscp_version
#   NSCP_VERSION=0.17.0 tests/integration/run.sh
#   tests/integration/run.sh -- --nocapture  # extra args go to `cargo test`
#
# To run the suite against an NSClient++ you started yourself, skip this
# script and set CHECK_NSCLIENT_IT_URL / CHECK_NSCLIENT_IT_PASSWORD directly
# (see README.md).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
version="${NSCP_VERSION:-$(tr -d '[:space:]' < "$root/.nscp_version")}"
arch="${NSCP_ARCH:-$(case "$(uname -m)" in aarch64|arm64) echo arm64;; *) echo amd64;; esac)}"
password="${NSCP_PASSWORD:-it-password}"
port="${NSCP_PORT:-8443}"
image="check_nsclient-it:${version}-${arch}"
container="check_nsclient-it-$$"

echo "==> Building ${image}"
docker build \
    --build-arg "NSCP_VERSION=${version}" \
    --build-arg "NSCP_ARCH=${arch}" \
    --build-arg "NSCP_PASSWORD=${password}" \
    -t "${image}" "$root/tests/integration"

cleanup() {
    echo "==> Stopping ${container}"
    docker logs --tail 50 "${container}" 2>/dev/null || true
    docker rm -f "${container}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "==> Starting ${container}"
docker run -d --name "${container}" -p "${port}:8443" "${image}" >/dev/null

echo "==> Waiting for https://127.0.0.1:${port}"
for _ in $(seq 1 60); do
    if curl -ksf -o /dev/null "https://127.0.0.1:${port}/api/v2/info" \
        || curl -ks -o /dev/null -w '%{http_code}' "https://127.0.0.1:${port}/api/v2/info" | grep -qE '^(401|403)$'; then
        break
    fi
    sleep 1
done

echo "==> Running integration tests against NSClient++ ${version}"
cd "$root"
CHECK_NSCLIENT_IT_URL="https://127.0.0.1:${port}" \
CHECK_NSCLIENT_IT_PASSWORD="${password}" \
CHECK_NSCLIENT_IT_USERNAME="${NSCP_USERNAME:-admin}" \
    cargo test --test integration -- "$@"
