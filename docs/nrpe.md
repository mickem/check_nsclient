# NRPE

`check_nsclient nrpe` runs checks over NRPE, the protocol `check_nrpe` speaks.
It talks to the `NRPEServer` module of NSClient++ and to any other NRPE daemon.

Unlike the `nsclient` commands it uses neither the REST API nor a profile:
there is no login, and the server decides by source address (and optionally by
client certificate) whom it answers.

```sh
check_nsclient nrpe -H host ping
check_nsclient nrpe -H host query check_cpu
check_nsclient nrpe -H host query check_drivesize "crit=free<10%" drive=c:
```

`ping` sends the `_NRPE_CHECK` command, which the server answers without
running anything. `query` runs a command; everything after its name is passed on
as arguments, which the server has to allow (`allow arguments = true`). A `!`
inside an argument splits it in two, as it does with `check_nrpe -a`.

## Nagios plugin behaviour

The command is meant to be used as a check command. It prints the plugin output
the server sent and exits with the status of the check: 0 OK, 1 WARNING,
2 CRITICAL, 3 UNKNOWN. Anything that prevents the check from running -- a
refused connection, a failed handshake, a timeout -- is printed as
`UNKNOWN: <reason>` on stdout and also exits with 3.

```
define command {
    command_name check_nrpe
    command_line /usr/bin/check_nsclient nrpe -H $HOSTADDRESS$ --ca /etc/nagios/nscp-ca.pem query $ARG1$
}
```

`--output json`, `yaml` and `csv` give `command`, `result` (the status by name)
and `output` instead; the exit code is the same.

## Options

| Option                    | Default | Purpose                                                        |
| ------------------------- | ------- | -------------------------------------------------------------- |
| `-H`, `--host`            |         | Host to connect to (required)                                  |
| `-p`, `--port`            | `5666`  | Port the NRPE server listens on                                |
| `-t`, `--timeout-s`       | `10`    | Seconds allowed for the whole exchange                         |
| `--protocol-version`      | `2`     | `2` or `4`, see below                                          |
| `--payload-length`        | `1024`  | Buffer size of a version 2 packet; must match the server       |
| `--no-tls`                |         | Connect in plain text                                          |
| `-k`, `--insecure`        |         | Do not verify the server certificate                           |
| `--ca FILE`               |         | CA certificate(s) to trust besides the system's (PEM)          |
| `--certificate FILE`      |         | Client certificate to present (PEM)                            |
| `--certificate-key FILE`  |         | Private key of the client certificate (PEM)                    |

They go before the sub command: `nrpe -H host -k query check_cpu`. Add `-d` or
`-dd` before `nrpe` to see the connection and the packets on stderr.

## TLS

TLS is on by default and the server certificate is verified against the system
trust store, plus whatever `--ca` adds. NSClient++ generates a self-signed
certificate on install, so out of the box one of these is needed:

- `--ca` pointing at a copy of the server's `certificate.pem` (or at the CA that
  issued it, if you replaced it), or
- `--insecure`, which still encrypts but accepts any certificate.

A server set up with `verify mode = peer-cert` -- what `nscp nrpe install`
writes unless told otherwise -- only answers clients that present a certificate
issued by its CA; pass it with `--certificate` and `--certificate-key`.

Not supported: legacy NRPE "encryption" with anonymous Diffie-Hellman ciphers,
which is what `insecure = true` in NSClient++ and a stock `check_nrpe` without
certificate options use. It offers no authentication and current TLS libraries no
longer implement it. Use certificates, or `--no-tls` against a server with
TLS turned off, on a network you trust.

## Protocol versions

Version 2 uses packets of a fixed size, 1024 bytes of payload unless both sides
are configured otherwise (`payload length` on the server, `--payload-length`
here). Every NRPE server understands it, and NSClient++ extends it: an answer
that does not fit is sent as several packets (`extended response`, on by
default), which this client joins. That makes version 2 the default.

Version 4 packets carry their own length. Use `--protocol-version 4` for other
NRPE daemons when answers are cut off at 1023 bytes, or when the query itself
is longer than that. NSClient++ 0.18 answers version 4 queries only as long as
they fit in a packet of the stock size (about a thousand bytes of command and
arguments); its answers are not limited.

## When it does not work

| Output                                                    | Usual cause                                                                                    |
| --------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `the server closed the connection without answering`      | This host is not in `allowed hosts`; TLS on one side only; another `payload length`             |
| `TLS handshake ... failed: invalid peer certificate`      | The certificate is not trusted (`--ca`, `--insecure`) or does not match the host name used      |
| `the server demands a client certificate`                 | `verify mode = peer-cert` on the server (`--certificate`, `--certificate-key`)                  |
| `no answer ... within N seconds`                          | A firewall drops the packets; a slow check (`--timeout-s`); another `payload length`            |
| `Request contained arguments (not currently allowed ...)` | `allow arguments` is off on the server                                                         |
| `Request arguments contained illegal metachars!`          | An argument holds one of ``|`&><'"\[]{}`` and `allow nasty characters` is off                   |
