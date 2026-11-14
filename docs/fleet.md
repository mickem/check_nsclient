# Explaining errors with a fleet server

The `fleet` commands talk to an NSClient++ fleet server rather than to one
agent. The fleet server keeps each host's latest metrics snapshot (sent by the
agent when a check fails) and its inventory facts. `fleet explain` hands it an
error from your monitoring system and prints an explanation of that error
against that evidence.

The fleet server does not judge how serious the error is — your monitoring
system already has. It explains *why*, using what the host looked like.
Explanations must be turned on for the tenant (Settings in the fleet console);
see `docs/explaining-errors.md` in the fleet server for providers and privacy.

## Signing in

Create an API key in the fleet console (API keys), then store it with the
server's URL as a fleet profile:

```sh
check_nsclient fleet login --url https://fleet.example.com
check_nsclient fleet login prod --url https://fleet.example.com --ca fleet-ca.pem
```

The key is prompted for unless `--api-key` is given, and stored in the system
keyring. The URL and key are checked against the server before anything is
stored. The first fleet profile becomes the default; use `fleet -p prod …` to
pick another. Fleet profiles are separate from NSClient++ profiles, so the
same id can be used for both.

`check_nsclient fleet logout prod` removes a profile and its key.

Without a profile, pass `--url` and `--api-key`, or set `FLEET_URL` and
`FLEET_API_KEY`. A flag overrides the profile's value field by field, so a
profile can supply the URL while `FLEET_API_KEY` supplies the key.

## Explaining an error

```sh
check_nsclient fleet explain --host web01 "PROBLEM: web01/disk_c is CRITICAL: C:\ used 95.2% > 90%"
check_nsclient fleet explain --host web01 --check disk_c "C:\ used 95.2% > 90%"
some-alert-hook | check_nsclient fleet explain --host web01
```

- `--host` is the host's name as the fleet knows it, or its id. A name shared
  by several hosts is refused with their ids.
- `--check` is a command (`check_drivesize`) or an alias (`disk_c`). Without
  it, a check named in the error text is used if there is one.
- The error is the remaining words, or standard input when there are none (or
  the only one is `-`).

The explanation uses the agent's latest report of the matched check, the
host's latest metrics snapshot and its facts. The output says which of those
were available.

From a Nagios notification command:

```
define command {
    command_name  notify-explain
    command_line  /usr/local/bin/check_nsclient fleet explain --host "$HOSTNAME$" --check "$SERVICEDESC$" "$SERVICEOUTPUT$"
}
```

Explaining waits on a language model, which can take tens of seconds; the
default `--timeout-s` is 120.

## Output

```sh
check_nsclient --output json fleet explain --host web01 "disk_c is CRITICAL"
check_nsclient --output yaml fleet explain --host web01 "disk_c is CRITICAL"
check_nsclient --output csv  fleet explain --host web01 "disk_c is CRITICAL"
```

Text output is the explanation followed by the evidence it was based on. JSON
and YAML are the server's answer as it was sent. CSV is one flat record.

## Errors

The server's reason is printed as it was given, with its status: an unknown
host (404), a hostname matching several hosts or explanations not enabled for
the tenant (409), the day's budget spent (429), or the model provider failing
(502). A rejected API key (401) asks you to sign in again.
