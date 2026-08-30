# NSClient++ under test for the check_nsclient integration suite.
#
# Installs the official Ubuntu 24.04 package for the requested release from
# https://github.com/mickem/nscp/releases, enables the REST API over HTTPS and
# runs the agent in the foreground (`nscp test`), the same way the nscp
# repository's own integration harness does.
#
#   docker build --build-arg NSCP_VERSION=0.18.0 -t check_nsclient-it tests/integration
#   docker run --rm -p 8443:8443 check_nsclient-it
#
# The REST password is baked in as a build argument (default "it-password")
# because this image only ever runs on a developer machine / CI runner and is
# discarded afterwards.
FROM ubuntu:24.04

ARG NSCP_VERSION
ARG NSCP_ARCH=amd64
ARG NSCP_PASSWORD=it-password

RUN test -n "${NSCP_VERSION}" || { echo "NSCP_VERSION build-arg is required" >&2; exit 1; }

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl python3-protobuf \
    && (DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends liblua5.3-0 \
        || DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends liblua5.4-0) \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL -o /tmp/nscp.deb \
        "https://github.com/mickem/nscp/releases/download/${NSCP_VERSION}/NSCP-${NSCP_VERSION}-ubuntu-24.04-${NSCP_ARCH}.deb" \
    && apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y /tmp/nscp.deb \
    && rm -rf /var/lib/apt/lists/* /tmp/nscp.deb

# Baseline configuration (see build.md "Dev build" in the nscp repository):
#  - generic check modules so `queries` / `modules` have something to work on,
#  - LUAScript so `scripts list-runtimes` reports a runtime,
#  - HTTPS REST API on 8443 with a CIDR allow-list ('*' does not resolve).
RUN nscp settings --load-all --list --add-defaults >/dev/null \
    && nscp settings --activate-module CheckHelpers CheckSystem CheckDisk CheckExternalScripts LUAScript \
    && nscp web install --https --allowed-hosts '0.0.0.0/0,::/0' --password "${NSCP_PASSWORD}"

EXPOSE 8443

CMD ["nscp", "test"]
