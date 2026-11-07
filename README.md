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
  state.json         # last-run times, streaks, running pid: written by the daemon (gitignored)
```

`--profile <dir>` or `WORK_AGENT_PROFILE` selects the folder. With neither, the
**current folder is the profile** when it looks like one (it has a `tasks/`
folder or a `config.toml`), so working inside a profile folder needs no path;
else the default is `~/.config/work-agent/default`. A path that does not exist
yet is created with `tasks/`, `workstations/` and the `.gitignore` above, ready
for `git init`. `examples/` is a complete profile you can run as is.

## Run

Inside a profile folder, every command works on it:

```sh
cd ~/dev/work-agent-configs/stay22
work-agent                          # install this folder as a service (see below)
work-agent daemon                   # ... or run the daemon here, in the foreground
work-agent list                     # this folder's tasks, no --profile needed
```

Anywhere else, with the default profile:

```sh
mkdir -p ~/.config/work-agent/default/tasks
cp examples/config.toml ~/.config/work-agent/default/           # global settings
cp examples/tasks/daily-standup-notes.toml ~/.config/work-agent/default/tasks/
cargo run -- check                 # validate config and every task file
cargo run -- list                  # schedule, last run, streak, next run, and what is due
cargo run -- run daily-standup-notes   # force one task now
cargo run -- run release-notes         # start a manual task (no schedule)
cargo run -- run                       # list runnable tasks: status, last/next run, harness/model, description
cargo run                          # daemon (same as `cargo run -- daemon`)
cargo run -- --profile ~/agent-profiles/work daemon   # another profile, e.g. a private repo checkout
```

## Run as a service

Bare `work-agent` **inside a profile folder** installs that folder as a systemd
user service and links the skills (see [Skills](#skills)) into it: the whole
deploy of a profile in one word, safe to rerun any time.

```sh
cd ~/dev/work-agent-configs/stay22
work-agent                          # -> work-agent-stay22.service, enabled and running
work-agent                          # again: reinstalled, skills relinked, nothing duplicated
```

It validates the profile first (a bad task file writes nothing), never removes
or restarts the daemons of other profiles, and prints the journal command. With
paths, `work-agent service <profile>...` installs the binary that runs it as
systemd user units, one per profile. Paths are scaffolded when missing. The
installed daemons become exactly the paths you pass: any other
`work-agent-*.service` is removed first, unless `--keep-others`:

```sh
cargo install --path .                                        # puts work-agent on your PATH
cd ~/dev/work-agent-configs/stay22 && work-agent              # install the folder you are in
work-agent service ~/.config/work-agent/default               # one daemon: work-agent-default.service
work-agent service ~/.config/wa ~/dev/some-private-config     # two daemons, the default one is removed
work-agent service ~/profiles/other --keep-others             # add a third, keep the rest
work-agent service --status                                   # every installed daemon
work-agent service --uninstall ~/dev/some-private-config
work-agent service --uninstall --all                          # remove every installed daemon
journalctl --user -fu work-agent-wa.service                   # logs
```

From a checkout, `cargo service ...` (alias in `.cargo/config.toml`) does the
same with a release build of the working tree. Units are named after the
profile folder, so two profiles must not share a folder name. Each unit
points at the binary that installed it, so rerun it (`work-agent` in the
folder, or `work-agent service <profile>`) after reinstalling or rebuilding; it
restarts the daemons. It captures your current `PATH` so `herdr` and the
harnesses resolve the same way they do in your shell. `work-agent service` with
no path installs, and with `--uninstall` removes, the profile in hand.

## Skills

The skills that belong to a profile (`add-task`, kept in this repo's
`.claude/skills/`) are not copied into it: installing a profile drops a
**symlink** to them, in both places a harness looks. `.agents/skills`, because
pi reads it from the working directory up through its ancestors, which covers
every task workstation under the profile; and `.claude/skills`, for claude. One
copy, edited in one place, true everywhere at once:

```sh
cd ~/dev/work-agent-configs/stay22 && work-agent
# skills: .../stay22/.agents/skills -> ~/dev/work-agent/.claude/skills (pi)
# skills: .../stay22/.claude/skills -> ~/dev/work-agent/.claude/skills (claude)
```

The links go into the profile's `.gitignore` (a one-time file to commit), so a
profile repository stays clean. A folder of your own in either place is left
alone with a warning, never deleted. `WORK_AGENT_SKILLS` points the links at
another skills folder when the binary was not built from this checkout.

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
| `defaults.fresh_workspace` | always create a new workspace labelled `cron:<profile>/<name>:<MM-DD HH:MM>` instead of reusing the task's (default `false`) |
| `exception_llm` | OpenAI-compatible endpoint used to evaluate `exception` rules |

Per task file: `prompt` (required for the `herdr` and `process` runners),
`command` (required for the `command` runner), optional `description`,
`schedule` (5 or 6 field cron), `harness`, `args`, `cwd` (defaults to the
task's workstation; a configured `cwd` must exist or the run is refused), `parallel`
(default false), `exception`, `runner`, `timeout_seconds`,
`close_workspace_on_done`, `fresh_workspace`, `enabled`.

```toml
# tasks/ci-watch.toml
schedule = "*/30 * * * *"
harness = "claude"
args = ["--model", "claude-sonnet-5-5"]
cwd = "~/projects/my-app"
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

## Status

`list`, and `run` with no name, show how each task's last executions went and
whether one is in flight right now:

```text
TASK                     SCHEDULE       HARNESS PAR   LAST RUN          NEXT RUN          DUE  STATUS
nightly-webdev-deploy    0 5 * * *      pi      false 2026-11-06 05:00  2026-11-07 05:00       3 successes in a row
hourly-mail-digest       0 * * * *      pi      false 2026-11-06 17:00  2026-11-06 18:00  DUE  2 failures in a row (running now)
manual-webdev-release    manual         claude  false never             -                      never ran
```

A streak counts consecutive runs with the same verdict and lives in
`state.json` (`streak`, `last_result`): three successes and then a failure
reads `1 failure in a row`, and a run the `exception` rule skipped leaves the
count alone. A task that only ever got skipped reads `never ran, skipped last`.
The `(running now)` suffix comes from the pid the run recorded in `state.json`,
checked against `/proc`, so a task left marked by a daemon that died does not
look busy forever.

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
4. If an agent named `<name>` is still alive it is prompted directly (a run is refused while it is still `working`). Otherwise the `cron:<profile>/<name>` workspace is reused when still open (a free pane, or a new split), and only then `herdr workspace create --cwd <cwd>`. Then `herdr agent start <name> --kind <harness> -- <args>` → `herdr agent prompt <name> "<prompt>" --wait`. With `fresh_workspace: true` every run gets its own `cron:<profile>/<name>:<MM-DD HH:MM>` workspace and agent.
   `command` runner tasks skip the agent entirely: the `command` line is sent to a shell pane of the same `cron:<profile>/<name>` workspace with `herdr pane run`, and the run settles when the pane's foreground process returns to its shell (polled via `herdr pane process-info`, bounded by `timeout_seconds`). The pane stays in your herdr session the whole time, so interactive prompts — e.g. `sudo`'s password — can be answered by hand.
5. The task is marked `running` in `state.json` — with its pid — before it starts, so a crash mid-run cannot retrigger it on restart, and its verdict then extends the task's streak in the state file the loop moves on from.
