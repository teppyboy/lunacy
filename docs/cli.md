# lunacy-cli

A command-line client for the Lunacy daemon, written for scripts and AI agents. It talks to the
daemon's HTTP API (`docs/api.md`) and never needs a terminal.

```sh
cargo install --path apps/lunacy-cli --locked
lunacy-cli schema        # the whole interface as JSON: commands, args, exit codes, output shapes
```

## Conventions

- **Connection.** `--server` / `LUNACY_SERVER` (default `http://127.0.0.1:7878`). Token from
  `LUNACY_TOKEN`, `--token-file` / `LUNACY_TOKEN_FILE`, or `~/.config/lunacy/api-token` when it
  exists. `--token` works too but is visible in `ps`. The CLI never prints the token.
- **Output.** JSON when stdout is not a terminal, or with `--json`. One JSON document per
  command; `logs --follow` prints NDJSON. Progress of `trigger --wait` and `wait` goes to stderr
  as NDJSON events (`{"event":"started","run_id":…}`, `{"event":"step","index":…,"status":…}`),
  so stdout stays parseable.
- **Errors.** In JSON mode, stderr gets `{"error":{"code","exit","message","http_status"}}`.

| Exit | Code | Meaning |
|---|---|---|
| 0 | `ok` | success; a waited-for run succeeded |
| 1 | `error` | anything else (bad response, invalid config, …) |
| 2 | `usage` | invalid arguments |
| 3 | `not_found` | unknown project, run or step (HTTP 404) |
| 4 | `auth` | missing or wrong token (HTTP 401/403) |
| 5 | `conflict` | not possible now, e.g. the project is already running (HTTP 409) |
| 6 | `unreachable` | the daemon did not answer |
| 7 | `run_failed` | a waited-for run ended as failure, error or cancelled |
| 8 | `timeout` | `--timeout` passed before the run finished |

## Commands

| Command | What it does |
|---|---|
| `status` | health, daemon system info, one summary per project |
| `doctor` | checks reachability, the token, and act / git / docker on the daemon host; exits non-zero on a problem |
| `projects`, `project <name>` | project state, as in `GET /api/projects` |
| `runs <project> [--limit N]` | runs, newest first |
| `run <id>` | one run with steps and act substeps |
| `logs <run-id> [--step N] [--tail N] [--follow]` | a step's log; without `--step`, the failed step (else the last started one) |
| `trigger <project> [--sha S] [--force] [--deploy-only] [--reason R] [--wait [--timeout S]]` | start a run; with `--wait`, print the finished run |
| `wait <run-id> [--timeout S]` | block until a run finishes |
| `cancel <run-id \| project>` | cancel a run, or a project's running and queued runs |
| `retry <run-id> [--reason R]` | run a finished run's commit again |
| `config get [--path a.b.0.c]` | the daemon's config, secrets masked |
| `config check <file>` | validate a config file locally, no daemon needed |
| `workflows <project>` | the project's workflows and jobs |
| `webhooks` | the GitHub webhook endpoint and its last delivery |
| `schema` | everything above, as JSON |

## Recipes

Run CI on the current upstream head and act on the result:

```sh
if lunacy-cli trigger frostedge --force --wait --timeout 3600 > run.json; then
  echo "green"
else
  case $? in
    7) jq -r '.failed_step' run.json ;;          # the run failed: which step?
    8) echo "still running after an hour" ;;
    *) cat >&2 ;;                                 # couldn't start it (auth, 409, down, ...)
  esac
fi
```

Why did the last run fail?

```sh
run=$(lunacy-cli project frostedge | jq -r '.last_run.id')
lunacy-cli run "$run" | jq '{status, failed_step, steps: [.steps[] | {name, status, substeps: [.substeps[] | select(.status != "success")]}]}'
lunacy-cli logs "$run" --tail 80 | jq -r '.log'   # defaults to the failed step
```

Watch a running step live:

```sh
lunacy-cli logs "$run" --follow | jq -r '.line'
```

Check a config edit before saving it:

```sh
lunacy-cli config check ~/.config/lunacy/config.toml || jq -r '.field + ": " + .message'
```
