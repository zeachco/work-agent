---
name: add-task
description: Add a scheduled agent task to a work-agent profile (one TOML file per task under <profile>/tasks/). Use when the user asks to add, schedule, or create a new task, cron, or recurring agent job in this repo.
---

# Add a task to work-agent

Tasks are not in this repo: they live in a **profile folder** — the folder you
are working in when it has a `tasks/` folder or a `config.toml`, otherwise
`--profile <dir>` / `WORK_AGENT_PROFILE`, otherwise
`~/.config/work-agent/default` (the user may name another one, e.g. a private
repo checkout with `work/` and `personal/` profiles). Each task is one TOML
file, `<profile>/tasks/<frequency>-<domain>-<task>.toml`, with examples in
`examples/tasks/`. Global settings live in `<profile>/config.toml`. The
schema is `Task` in `src/config.rs` and rejects unknown keys, so only use the
fields below. The task name is the file stem and must not appear in the file.

Every new task filename must follow `<frequency>-<domain>-<task>.toml`:
`<frequency>` describes the cadence (for example `nightly`, `weekly`, or
`manual`), `<domain>` identifies the area (for example `webdev` or `system`),
and `<task>` is a specific snake_case action. For example:
`nightly-webdev-ecotec_gitlab_issues_process_one.toml`.

## Steps

1. Find the profile (the current folder when it is one, else
   `$WORK_AGENT_PROFILE`, else `~/.config/work-agent/default`, unless the user
   names one) and look at its `tasks/` for the existing names and at a neighbour
   file for style.
2. Write `<profile>/tasks/<frequency>-<domain>-<task>.toml` using the naming convention above.
3. Validate: `work-agent check` (add `--profile <dir>` when you are not in the
   profile folder; `cargo run -- check` when running from a checkout).
4. Confirm the schedule: `work-agent list` (shows last / next run, the run
   status streak, and `manual` for unscheduled tasks).
5. Optionally force a run: `work-agent run <frequency>-<domain>-<task>`.

A running daemon picks the new file up on its next tick; no restart needed.

## Task fields

| field | required | meaning |
|---|---|---|
| file name | yes | `<frequency>-<domain>-<task>.toml`, with a stem matching `[a-z][a-z0-9_-]{0,31}`; also the herdr agent name |
| `schedule` | no | cron, 5 fields `m h dom mon dow` or 6 fields with leading seconds. Omit it for a **manual task**: never run by the daemon, only via `work-agent run <name>` |
| `prompt` | for `herdr`/`process` | what the agent does; supports placeholders (below) |
| `command` | for `command` | shell line sent straight to a herdr pane, no agent involved |
| `description` | no | one-line summary shown by `work-agent run` (no name), which lists all runnable tasks |
| `cwd` | no | directory the harness starts in (falls back to `defaults.cwd`) |
| `harness` | no | `pi` (default), `claude`, `codex`, ... anything `herdr agent start --kind` accepts |
| `args` | no | replaces `defaults.args` entirely, e.g. `["--model", "claude-sonnet-5-5"]` |
| `runner` | no | `herdr` (workspace + interactive agent, default), `process` (`harness -p prompt`) or `command` (run the `command` line directly in a herdr pane) |
| `parallel` | no | `true` lets it run alongside other tasks; default is strictly one at a time |
| `exception` | no | natural-language rule evaluated by the local LLM; a SKIP answer skips the run |
| `preflight` | no | shell line run from `cwd` before the run; a non-zero exit skips it (see Gates) |
| `validate` | no | shell line run from `cwd` after a run that looked good; a non-zero exit fails it |
| `timeout_seconds` | no | max wall-clock for one run (default 3600) |
| `close_workspace_on_done` | no | close the herdr workspace once the agent settles |
| `fresh_workspace` | no | default `false`: reruns reuse the live `<name>` agent, else the still-open `cron:<name>` workspace, else create one. `true` creates a new `cron:<name>:<MM-DD HH:MM>` workspace and agent on every run |
| `enabled` | no | default `true`; set `false` to keep a task without scheduling it |

Placeholders usable in `prompt`, `exception` and the gates: `${date}` (YYYY-MM-DD),
`${time}` (HH:MM), `${datetime}`, `${weekday}`, `${holidays}` (comma-joined
config list), `${task}` (task name), `${workstation}` (the task's scratch dir,
`<profile>/workstations/<name>/`).

## Gates (`preflight` / `validate`)

Each is one shell line, run as `sh -c '<line>'` from the task's `cwd`, where
the exit code is a verdict: `preflight` non-zero **skips** the run (nothing
ran, so the task's streak is untouched and it is tried again next tick),
`validate` non-zero **fails** it. Both share the task's `timeout_seconds` and
`cwd`, both accept the placeholders below, and both land trimmed to one line
in the log, `state.json` and `list` together with what they printed — so a gate
should print why it refused:

```toml
preflight = 'test -f ~/vpn-up || { echo "no vpn" >&2; exit 1; }'   # bash ./preflight.sh works too
validate = '''
npm run build --silent || { echo "build failed" >&2; exit 1; }
'''
```

Use `preflight` for a precondition the task cannot do anything about (nothing
to process, a dependency unreachable, a lock held by another job) and
`validate` for the proof the work is right — especially on the `herdr` runner,
whose `settled` status only says the agent stopped talking. A script can live
in the task's workstation and be called by relative path (`bash ./check.sh`),
but remember `memory.md` there is tracked in git while the rest is not.
`work-agent run <name> --ignore-gates` runs the task whatever both answer.

## Workstation (memory board)

Each task gets `<profile>/workstations/<name>/`, created on first run with a
`memory.md` (tracked in git when the profile is a repo, so never secrets) and a
`short_memory.md` (gitignored: secrets, sensitive matter, unimportant details).
The daemon prepends a note to every prompt asking the agent to read and update
both and to keep temp files and reusable scripts there
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
  is unreachable, the task runs anyway. A `preflight` is a hard gate: it runs
  first, and a non-zero exit really does skip the run.
- A gate runs from `cwd`, not from the profile or the repo: `bash ./hooks.sh`
  is the task's own `cwd/hooks.sh`. A gate that cannot start (typo, missing
  `cwd`) counts as a refusal, never as a pass.
- Prompts are passed as a single argument (no shell), so quotes are safe.
  Use a literal multi-line string (`'''...'''`) so backticks, quotes and
  `${...}` need no escaping; the newline right after the opening `'''` is dropped.
- A rerun is refused while the task's previous agent is still `working`;
  use `fresh_workspace: true` (or `parallel`) if overlapping runs are wanted.
- Never write tasks, config or workstation files into this repo; they belong
  to the profile. Keep `examples/` generic and free of secrets.
- A profile folder installed with `work-agent` has `.agents/skills` and
  `.claude/skills` symlinks into this repo, which is how this skill is
  reachable from there. Edit the skills here, never through the link.
- Anything the agent should keep across runs belongs in its workstation, not
  in the working tree it operates on.
