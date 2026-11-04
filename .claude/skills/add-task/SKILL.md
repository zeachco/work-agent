---
name: add-task
description: Add a scheduled agent task to this work-agent project (one TOML file per task under tasks/). Use when the user asks to add, schedule, or create a new task, cron, or recurring agent job in this repo.
---

# Add a task to work-agent

Each task is one TOML file, `tasks/<frequency>-<domain>-<task>.toml` (local,
gitignored), with examples in `examples/tasks/`. Global settings live in
`config.toml`. The schema is `Task` in `src/config.rs` and rejects unknown
keys, so only use the fields below. The task name is the file stem and must
not appear in the file.

Every new task filename must follow `<frequency>-<domain>-<task>.toml`:
`<frequency>` describes the cadence (for example `nightly`, `weekly`, or
`manual`), `<domain>` identifies the area (for example `webdev` or `system`),
and `<task>` is a specific snake_case action. For example:
`nightly-webdev-ecotec_gitlab_issues_process_one.toml`.

## Steps

1. Look at `tasks/` for the existing names and at a neighbour file for style.
2. Write `tasks/<frequency>-<domain>-<task>.toml` using the naming convention above.
3. Validate: `cargo run -- check`.
4. Confirm the schedule: `cargo run -- list` (shows last / next run; manual tasks show `manual`).
5. Optionally force a run: `cargo run -- run <frequency>-<domain>-<task>`.

A running daemon picks the new file up on its next tick; no restart needed.

## Task fields

| field | required | meaning |
|---|---|---|
| file name | yes | `<frequency>-<domain>-<task>.toml`, with a stem matching `[a-z][a-z0-9_-]{0,31}`; also the herdr agent name |
| `schedule` | no | cron, 5 fields `m h dom mon dow` or 6 fields with leading seconds. Omit it for a **manual task**: never run by the daemon, only via `cargo run -- run <name>` |
| `prompt` | for `herdr`/`process` | what the agent does; supports placeholders (below) |
| `command` | for `command` | shell line sent straight to a herdr pane, no agent involved |
| `description` | no | one-line summary shown by `cargo run -- run` (no name), which lists all runnable tasks |
| `cwd` | no | directory the harness starts in (falls back to `defaults.cwd`) |
| `harness` | no | `pi` (default), `claude`, `codex`, ... anything `herdr agent start --kind` accepts |
| `args` | no | replaces `defaults.args` entirely, e.g. `["--model", "claude-sonnet-5-5"]` |
| `runner` | no | `herdr` (workspace + interactive agent, default), `process` (`harness -p prompt`) or `command` (run the `command` line directly in a herdr pane) |
| `parallel` | no | `true` lets it run alongside other tasks; default is strictly one at a time |
| `exception` | no | natural-language rule evaluated by the local LLM; a SKIP answer skips the run |
| `timeout_seconds` | no | max wall-clock for one run (default 3600) |
| `close_workspace_on_done` | no | close the herdr workspace once the agent settles |
| `fresh_workspace` | no | default `false`: reruns reuse the live `<name>` agent, else the still-open `cron:<name>` workspace, else create one. `true` creates a new `cron:<name>:<MM-DD HH:MM>` workspace and agent on every run |
| `enabled` | no | default `true`; set `false` to keep a task without scheduling it |

Placeholders usable in `prompt` and `exception`: `${date}` (YYYY-MM-DD),
`${time}` (HH:MM), `${datetime}`, `${weekday}`, `${holidays}` (comma-joined
config list), `${task}` (task name), `${workstation}` (the task's scratch dir,
`workstations/<name>/`).

## Workstation (memory board)

Each task gets a gitignored `workstations/<name>/` created on first run with a
`memory.md`. The daemon prepends a note to every prompt asking the agent to read
and update `memory.md` and to keep temp files and reusable scripts there
(e.g. `workstations/say-hello/format.py`). You do not need to mention it in the
prompt, but use `${workstation}` when the task should write a specific file
there, e.g. `"prompt": "... append today's summary to ${workstation}/log.md"`.
Delete the directory to reset a task's memory.

## Example

`tasks/weekly-webdev-dep_audit.toml`:

```toml
schedule = "0 8 * * 1"
harness = "claude"
args = ["--model", "claude-sonnet-5-5"]
cwd = "/Users/jane/projects/my-app"
exception = "Skip on holidays. Today is ${weekday} ${date}; holidays: ${holidays}."
prompt = '''
Run `npm audit` and summarize actionable findings into audit-${date}.md.'''
```

## Manual task example

Same fields, no `schedule`; the user starts it on demand. `tasks/manual-webdev-release_notes.toml`:

```toml
description = "Draft release notes for my-app since the last tag"
harness = "claude"
args = ["--model", "claude-sonnet-5-5"]
cwd = "/Users/jane/projects/my-app"
prompt = '''
Draft release notes from the commits since the last tag into release-notes-${date}.md.'''
```

## Command task example

No agent: the `command` line runs in a shell pane of the task's
`cron:<name>` workspace. Use it for scripts that need the user's hands
(e.g. a `sudo` password) — the pane stays in the herdr session the whole
run. `prompt` is not used by this runner.

`tasks/nightly-system-log_rotate.toml`:

```toml
schedule = "0 3 * * *"
runner = "command"
command = "bash ~/scripts/rotate-logs.sh"
description = "Rotate app logs"
```

## Gotchas

- When the user wants a task they trigger themselves ("on demand", "manual",
  "not on a cron"), omit `schedule` rather than using `enabled: false`:
  disabled tasks cannot be run at all, manual ones can.

- A scheduled task that has never run is due immediately at the next tick.
  Set `"enabled": false` while drafting if that is unwanted.
- `args` is a full override, not a merge: repeat `--model` if you need it.
- `exception` is only a guard; if the local LLM at `exception_llm.base_url`
  is unreachable, the task runs anyway.
- Prompts are passed as a single argument (no shell), so quotes are safe.
  Use a literal multi-line string (`'''...'''`) so backticks, quotes and
  `${...}` need no escaping; the newline right after the opening `'''` is dropped.
- A rerun is refused while the task's previous agent is still `working`;
  use `fresh_workspace: true` (or `parallel`) if overlapping runs are wanted.
- `tasks/` and `config.toml` are gitignored; keep `examples/tasks/` and
  `config.example.toml` generic and free of secrets.
- `workstations/` is gitignored too: anything the agent should keep belongs there, not in the working tree.
