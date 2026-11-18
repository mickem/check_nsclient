# Failed checks

The `nsclient failures` commands read the agent's log of the last checks that
did not come back OK. They also show what the host looked like when each check
failed: a metrics snapshot and a set of diagnostics tables such as the top
processes. This needs an NSClient++ build with the failed-check log
(`/api/v2/failures`). An older agent, including the 0.18.0 the integration
suite pins, answers `404`, and the command reports that the agent has no
failed-check log.

```sh
check_nsclient nsclient -p prod failures list
check_nsclient nsclient -p prod failures list --status critical --limit 10
check_nsclient nsclient -p prod failures list --command check_drivesize --long
check_nsclient nsclient -p prod failures context 12
check_nsclient nsclient -p prod failures context 12 --section processes.top_cpu
check_nsclient nsclient -p prod failures status
check_nsclient nsclient -p prod failures clear
```

| Command | Endpoint | Privilege |
|---------|----------|-----------|
| `list` | `GET /api/v2/failures` | `failures.list` |
| `status` | `GET /api/v2/failures?limit=1` | `failures.list` |
| `context <index>` | `GET /api/v2/failures/{index}/context` | `failures.list` |
| `clear` | `DELETE /api/v2/failures` | `failures.delete` |

The built-in roles other than `full` do not have these privileges. A
monitoring console needs a role that grants `failures.list`.

## Listing failures

`list` shows the newest failure first. The filters run on the agent:

| Option | Meaning |
|--------|---------|
| `--status` | Comma-separated `ok`, `warning`, `critical`, `unknown` (or `0`-`3`). |
| `--command` | Only failures of this command or alias (case-insensitive). |
| `--since <INDEX>` | Only failures recorded after this index. |
| `--limit` | At most this many, applied after the other filters. |

The text table shows `index`, `date`, `result`, `command` and `message`. With
`--long` (or `--output-long`) it also shows:

- `age`, `subject` (who ran the check, e.g. `NRPEServer` or `WEBServer:admin`)
  and `depth` (`1+` for a check that another check ran)
- the recorded `arguments`, or `(not recorded)` when the agent keeps none
- the performance data
- the state of the `context` and `diagnostics` snapshots, e.g.
  `captured (5s before)` or `evicted`

An empty text listing says `No failed checks`, or that the log is disabled on
the agent when it is.

JSON and YAML keep the agent's whole envelope, including its configuration and
`last_index`. To poll, store `last_index` and pass it back as `--since`. If
`last_index` advanced by more than the number of entries you received, the
log's size limit evicted the ones in between:

```sh
check_nsclient --output json nsclient -p prod failures list --since 41 | jq '.last_index, .entries[].message'
```

## What the host looked like

`context <index>` shows the snapshots stored with one failure. Get the index
from `list`. The text output has three parts:

1. A summary: which snapshot was stored, how old it was when the check failed,
   and which diagnostics sections it has.
2. The metrics, one row per metric, in the same layout as
   `metrics show --meta`. Add `--long` for the labels and help text.
3. One table per diagnostics section, with its source and capture time.
   Units are shown in the column headers.

An entry without a snapshot is still answered. The output gives the reason,
for example `No metrics captured (disabled)`. The possible states are
`disabled`, `evicted`, `too_large` and `none`; the
[REST reference](https://github.com/mickem/nscp/blob/main/docs/docs/api/rest/failures.md)
explains each one. An index the log no longer holds is an error.

`--section` selects a single part: `metrics`, or the name of a diagnostics
section. A CSV file holds only one table, so CSV output shows the metrics
unless `--section` names something else. JSON and YAML return the whole
document as the agent sent it. With `--section metrics` they return the
`{metrics, metadata}` pair that `metrics show --meta` produces, and with any
other section name they return that section.

```sh
check_nsclient --output csv nsclient -p prod failures context 12 --section processes.top_cpu > top_cpu.csv
```

## Configuration

`status` shows how the log is set up on the agent: whether it is enabled, how
many entries it keeps, which statuses it records, how arguments are recorded,
and whether metrics and diagnostics are captured, including the snapshot
budget. The log is configured on the agent under
`[/settings/core/failed checks]`. Metrics and diagnostics capture are off by
default (`capture metrics`, `capture diagnostics`).

`clear` empties the log and reports how many entries it removed. `last_index`
is not reset, so a poller's `--since` value is still valid after a clear.
