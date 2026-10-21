# work-agent

A small daemon that cycles through scheduled agent tasks and runs each one
through a coding harness (`pi` by default, `claude`, or anything `herdr`
knows how to start) inside its own `herdr` workspace. Only one task runs at a
time unless it opts into `parallel`.

## Run

```sh
cp crons.example.json crons.json   # edit tasks
cargo run -- check                 # validate config
cargo run -- list                  # show schedule / last run / next run
cargo run -- run daily-standup-notes   # force one task now
cargo run                          # daemon (same as `cargo run -- daemon`)
```

`--config <path>` or `CRONS_CONFIG` selects the config. `RUST_LOG=debug`
shows every herdr call. The daemon must itself run inside a herdr session
(or on a machine where `herdr` can reach the running server) when tasks use
the default `herdr` runner.

## Config

| key | meaning |
|---|---|
| `tick_seconds` | how often the loop looks for due tasks |
| `state_file` | JSON file storing last-run timestamps (relative to the config) |
| `holidays` | dates exposed to prompts as `${holidays}` |
| `defaults.harness` | `pi`, `claude`, `codex`, ... (any `herdr agent start --kind`) |
| `defaults.args` | harness arguments, e.g. `["--model", "llamacpp/gemma-4-E4B-it"]` |
| `defaults.runner` | `herdr` (workspace + interactive agent) or `process` (`harness -p prompt`) |
| `defaults.timeout_seconds` | max wall-clock per run |
| `defaults.close_workspace_on_done` | close the herdr workspace once the agent settles |
| `exception_llm` | OpenAI-compatible endpoint used to evaluate `exception` rules |

Per task: `name`, `schedule` (5 or 6 field cron), `prompt`, optional
`harness`, `args`, `cwd`, `parallel` (default false), `exception`, `runner`,
`timeout_seconds`, `close_workspace_on_done`, `enabled`.

Prompts and exception rules accept `${date}`, `${time}`, `${datetime}`,
`${weekday}`, `${holidays}`, `${task}`.

## How a run works

1. A task is due when a cron tick has elapsed since `last_run` (never-run tasks are due immediately).
2. If it has an `exception`, the rendered rule is sent to the local model, which answers `SKIP` or `RUN`. An unreachable model never blocks a run.
3. `herdr workspace create --cwd <cwd>` → `herdr agent start <name> --kind <harness> -- <args>` → `herdr agent prompt <name> "<prompt>" --wait`.
4. The outcome is written to the state file and the loop moves to the next task.
