# Integration tests

End-to-end tests that run the **built `check_nsclient` binary** against a **real
NSClient++** and exercise every CLI feature: login/refresh/logout, profiles,
ping/version, modules, queries (incl. Nagios exit codes), logs, scripts,
settings, metrics and all output formats. They mirror the harness in the
[nscp repository](https://github.com/mickem/nscp/tree/master/tests) but stay
in Rust/cargo.

The suite lives in [`../integration.rs`](../integration.rs). It is **skipped**
(every test passes with a notice) unless `CHECK_NSCLIENT_IT_URL` is set, so a
plain `cargo test` never needs Docker.

## Requirements

- Docker (Docker Desktop with Linux containers on Windows)
- Rust toolchain
- `curl` (used by `run.sh` to wait for the server)

## Quick start

```sh
tests/integration/run.sh                       # NSClient++ version from .nscp_version
NSCP_VERSION=0.17.0 tests/integration/run.sh   # test against another release
tests/integration/run.sh -- --nocapture        # extra args go to `cargo test`
```

```powershell
tests\integration\run.ps1
$env:NSCP_VERSION = "0.17.0"; tests\integration\run.ps1
```

The script builds `tests/integration/Dockerfile` (Ubuntu 24.04 + the official
`.deb` from the nscp GitHub release), starts it on port 8443, runs
`cargo test --test integration` and removes the container again.

| Variable        | Default              | Purpose                                  |
| --------------- | -------------------- | ---------------------------------------- |
| `NSCP_VERSION`  | `.nscp_version` file | NSClient++ release to download and test  |
| `NSCP_ARCH`     | host arch            | `amd64` or `arm64` package               |
| `NSCP_PASSWORD` | `it-password`        | REST password baked into the image       |
| `NSCP_PORT`     | `8443`               | Host port the container is published on  |

## Against a server you started yourself

Set the target directly and run the test binary; nothing is built or started:

```sh
CHECK_NSCLIENT_IT_URL=https://127.0.0.1:8443 \
CHECK_NSCLIENT_IT_USERNAME=admin \
CHECK_NSCLIENT_IT_PASSWORD=secret \
  cargo test --test integration
```

The server needs the REST API (WEBServer) enabled with the `CheckHelpers`,
`CheckSystem`, `CheckDisk`, `CheckExternalScripts` and `LUAScript` modules
loaded — see the Dockerfile for the exact `nscp settings` / `nscp web install`
commands.

## What the tests touch

- Configuration is isolated: every client runs with `APPDATA` /
  `XDG_CONFIG_HOME` / `HOME` pointing at a temp directory, so your own profiles
  are never read or modified.
- Tokens are stored in the **real OS keyring** under profile ids unique to the
  run (`it-<pid>-<n>`, cleaned up on drop) plus one stable shared profile
  (`check_nsclient-it`) that is overwritten on every run.
- The NSClient++ instance is mutated by a few tests (a module is loaded and
  unloaded again, a setting under `/settings/check_nsclient-it` is written and
  saved, log counters are reset). Use a throw-away instance.

## CI

`.github/workflows/integration-tests.yml` runs the same flow on `ubuntu-latest`.
It is called from the feature and main build workflows and can be started
manually (*Actions → Integration tests → Run workflow*) with a different
`nscp-version`. On Linux the keyring is backed by a `gnome-keyring` daemon
started inside `dbus-run-session`, because GitHub runners have no desktop
session to provide a Secret Service.
