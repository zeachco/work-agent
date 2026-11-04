# work-agent

A small daemon that cycles through scheduled agent tasks and runs each one
either through a coding harness (`pi` by default, `claude`, or anything
`herdr` knows how to start) inside its own `herdr` workspace, or as a plain
shell command in a herdr pane. Only one task runs at a time unless it opts
into `parallel`.

## Profiles

Nothing personal lives in this repo. Everything the daemon reads or writes
sits in a **profile folder** that you point it at, so it can be its own
(private) git repository, and one checkout can hold several profiles
(`work/`, `personal/`, ...) with each machine, or each daemon, pointed at a
different one:

```
<profile>/
  .gitignore         # state and short memories, written when the folder is created
  config.toml        # global settings, optional (built-in defaults otherwise)
  tasks/<name>.toml  # one file per task; the file stem is the task name
  workstations/      # per-task memory boards, created by the daemon
  state.json         # last-run timestamps, written by the daemon (gitignored)
```

`--profile <dir>` or `WORK_AGENT_PROFILE` selects the folder; the default
is `~/.config/work-agent/default`. A path that does not exist yet is created
with `tasks/`, `workstations/` and the `.gitignore` above, ready for
`git init`. `examples/` is a complete profile you can run as is.

## Run

```sh
mkdir -p ~/.config/work-agent/default/tasks
cp examples/config.toml ~/.config/work-agent/default/           # global settings
cp examples/tasks/daily-standup-notes.toml ~/.config/work-agent/default/tasks/
cargo run -- check                 # validate config and every task file
cargo run -- list                  # show schedule / last run / next run
cargo run -- run daily-standup-notes   # force one task now
cargo run -- run release-notes         # start a manual task (no schedule)
cargo run -- run                       # list runnable tasks: last/next run, harness/model, description
cargo run                          # daemon (same as `cargo run -- daemon`)
cargo run -- --profile ~/agent-profiles/work daemon   # another profile, e.g. a private repo checkout
```

## Run as a service

`cargo service` (an alias in `.cargo/config.toml`) builds the release
binary and installs it as a systemd user unit for one profile. The profile
path is required and is scaffolded when missing; one unit per profile, so
`work/` and `personal/` can each have their own daemon:

```sh
cargo service install ~/.config/work-agent/default   # -> work-agent-default.service, enabled + started
cargo service install ~/agent-profiles/work          # -> work-agent-work.service
cargo service status ~/.config/work-agent/default
cargo service uninstall ~/.config/work-agent/default
journalctl --user -fu work-agent-default.service      # logs
```

`--name <suffix>` overrides the unit name when two profiles share a folder
name. The unit points at the binary that installed it
(`target/release/work-agent`), so rerun `install` after rebuilding, and it
captures your current `PATH` so `herdr` and the harnesses resolve the same way
they do in your shell.

`RUST_LOG=debug` shows every herdr call. The daemon must itself run inside a
herdr session (or on a machine where `herdr` can reach the running server)
when tasks use the default `herdr` runner.

## Config

`<profile>/config.toml` holds the global settings; each task is its own
`<profile>/tasks/<name>.toml` (examples in `examples/tasks/`). The file
stem is the task name. The daemon reloads on its next tick when any of these
files is added, changed or removed.

| key | meaning |
|---|---|
| `tick_seconds` | how often the loop looks for due tasks |
| `state_file` | JSON file storing last-run timestamps, default `state.json` (relative to the profile) |
| `workstations_dir` | per-task scratch root, default `workstations` (relative to the profile) |
| `tasks_dir` | folder of task files, default `tasks` (relative to the profile) |
| `holidays` | dates exposed to prompts as `${holidays}` |
| `defaults.harness` | `pi`, `claude`, `codex`, ... (any `herdr agent start --kind`) |
| `defaults.args` | harness arguments, e.g. `["--model", "llamacpp/gemma-4-E4B-it"]` |
| `defaults.runner` | `herdr` (workspace + interactive agent), `process` (`harness -p prompt`) or `command` (run a shell line directly in a herdr pane, no agent) |
| `defaults.timeout_seconds` | max wall-clock per run |
| `defaults.close_workspace_on_done` | close the herdr workspace once the agent settles |
| `defaults.fresh_workspace` | always create a new workspace labelled `cron:<name>:<MM-DD HH:MM>` instead of reusing the task's (default `false`) |
| `exception_llm` | OpenAI-compatible endpoint used to evaluate `exception` rules |

Per task file: `prompt` (required for the `herdr` and `process` runners),
`command` (required for the `command` runner), optional `description`,
`schedule` (5 or 6 field cron), `harness`, `args`, `cwd`, `parallel`
(default false), `exception`, `runner`, `timeout_seconds`,
`close_workspace_on_done`, `fresh_workspace`, `enabled`.

```toml
# tasks/ci-watch.toml
schedule = "*/30 * * * *"
harness = "claude"
args = ["--model", "claude-sonnet-5-5"]
cwd = "/Users/jane/projects/my-app"
parallel = true
prompt = '''
Check the latest CI run on main with `gh run list`; if it failed,
investigate and write findings to ci-report-${date}.md.'''
```

A task without `schedule` is a **manual task**: it is configured like any
other but the daemon never starts it. Only `work-agent run <name>` does.
`list` shows it as `manual`. `work-agent show` opens an fzf picker over the
task files (with the file as preview) and opens the pick in `$VISUAL`,
`$EDITOR`, or `nvim`.

Prompts and exception rules accept `${date}`, `${time}`, `${datetime}`,
`${weekday}`, `${holidays}`, `${task}`, `${workstation}`.

## Workstations

Every task owns `<profile>/workstations/<name>/`, a memory board that
survives across runs:

```
workstations/<name>/memory.md        # notes the agent keeps for its next run (tracked: no secrets, ever)
workstations/<name>/short_memory.md  # secrets, sensitive matter, unimportant details (gitignored)
workstations/<name>/...              # temp files and reusable artifacts, e.g. say-hello/format.py (gitignored)
```

Before each run the daemon creates the directory (and stub memory files on
first run) and prepends a note to the prompt telling the agent to read and
update both memories, to keep secrets out of `memory.md`, and to drop temp
files and scripts there instead of in the working tree. Use `${workstation}` in a prompt to reference it explicitly.
Delete the directory to reset a task's memory.

## Addons

`addons/` holds self-contained modules a prompt can call from any harness,
including the local-model `pi` harness that has no MCP connectors. Each one
is a dependency-free CLI, read-only by default, with its credentials under
`~/.config/work-agent/<addon>/`. See [addons/README.md](addons/README.md);
the first is `google-calendar` (today / week / range / search as text or JSON).

## How a run works

1. A task is due when a cron tick has elapsed since `last_run` (never-run tasks are due immediately).
2. If it has an `exception`, the rendered rule is sent to the local model, which answers `SKIP` or `RUN`. An unreachable model never blocks a run.
3. `<profile>/workstations/<name>/` is created if missing and the prompt gets the workstation preamble.
4. If an agent named `<name>` is still alive it is prompted directly (a run is refused while it is still `working`). Otherwise the `cron:<name>` workspace is reused when still open (a free pane, or a new split), and only then `herdr workspace create --cwd <cwd>`. Then `herdr agent start <name> --kind <harness> -- <args>` → `herdr agent prompt <name> "<prompt>" --wait`. With `fresh_workspace: true` every run gets its own `cron:<name>:<MM-DD HH:MM>` workspace and agent.
   `command` runner tasks skip the agent entirely: the `command` line is sent to a shell pane of the same `cron:<name>` workspace with `herdr pane run`, and the run settles when the pane's foreground process returns to its shell (polled via `herdr pane process-info`, bounded by `timeout_seconds`). The pane stays in your herdr session the whole time, so interactive prompts — e.g. `sudo`'s password — can be answered by hand.
5. The outcome is written to the state file and the loop moves to the next task.
