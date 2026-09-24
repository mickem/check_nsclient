# Host facts

The `nsclient facts` commands read the host inventory exposed by
[NSClient++ PR #1559](https://github.com/mickem/nscp/pull/1559). Use an agent
build that includes this API and an existing login profile:

```sh
check_nsclient nsclient -p prod facts show
check_nsclient nsclient -p prod facts show os
check_nsclient nsclient -p prod facts show os.family
check_nsclient nsclient -p prod facts refresh
```

`show` uses `GET /api/v2/facts`, passing the optional dotted path as the
`path` query parameter. It reads the stored snapshot without collecting.
`refresh` uses `POST /api/v2/facts/commands/refresh` to ask all enabled
producers to collect now, then displays the snapshot returned by that call.
The respective server privileges are `facts.get` and `facts.refresh`.

As with other commands, omit `-p prod` to use the default profile. Connection,
authentication, TLS and timeout options work the same way; for a slow
collection use, for example, `nsclient -p prod --timeout-s 120 facts refresh`.

## Output

```sh
check_nsclient --output json nsclient -p prod facts show
check_nsclient --output yaml nsclient -p prod facts show os
check_nsclient --output csv nsclient -p prod facts show hardware
check_nsclient --output-style markdown nsclient -p prod facts refresh
```

JSON and YAML retain the response envelope: `revision`, `collected`, `path`,
`found`, `enabled`, `errors`, `gathered` and `facts`. Objects, arrays, numbers
and booleans inside `facts` keep their types. To extract just the requested
value in a shell with jq:

```sh
check_nsclient --output json nsclient -p prod facts show os.family | jq -r '.facts'
```

Text and CSV have `path`, `value`, `gathered`, `status` and `error` columns.
Nested objects become dotted paths; lists stay JSON in a single value cell,
preserving record ids and order. Text also shows the revision and last
collection round. CSV contains only the table, suitable for piping or saving.

`gathered` is when a producer actually read the values from the host. It can
be older than `collected`, which is when the last collection round finished.
If a producer supplies no gathered time, the table falls back to `collected`.
A failed collection with retained values is marked `stale` with its error;
an enabled set without values is shown as `not collected`.

A missing path (`found: false`) or an empty inventory is a successful result.
An empty inventory prints `No facts collected` in text, keeps the envelope in
JSON/YAML and produces no CSV rows. HTTP failures, including a 404 from an
agent without the facts API, and malformed responses fail the command.
Per-set collection errors remain in the successful response so retained
inventory is still available to callers.

## Enabling collection

Facts are opt-in on the agent. Enable the desired sets in the producing
module's configuration, for example for CheckSystem on Windows:

```ini
[/settings/system/windows/facts]
os = true
hardware = true
```

On Linux the section is `/settings/system/unix/facts`. The collection interval
is controlled by `/settings/facts` → `interval` (one hour by default).
Refreshing does not enable any sets. See the
[API documentation on the branch](https://github.com/mickem/nscp/blob/84174ddc1bad4a4ba605ae24fe33e2d54e988109/docs/docs/api/rest/facts.md)
for the response contract and producer configuration.
