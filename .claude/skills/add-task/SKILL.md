---
name: add-task
description: Add a scheduled agent task to this work-agent project (crons.json). Use when the user asks to add, schedule, or create a new task, cron, or recurring agent job in this repo.
---

# Add a task to work-agent

Tasks live in the `tasks` array of `crons.json` (tracked in git) and are
documented by `crons.example.json`. The schema is `Task` in `src/config.rs`
and rejects unknown keys, so only use the fields below.

## Steps

1. Read `crons.json` (copy `crons.example.json` to it if missing).
2. Append a task object to `tasks`.
3. Validate: `cargo run -- check`.
4. Confirm the schedule: `cargo run -- list` (shows last / next run; manual tasks show `manual`).
5. Optionally force a run: `cargo run -- run <name>`.

The running daemon reloads nothing: restart it after editing the config.

## Task fields

| field | required | meaning |
|---|---|---|
| `name` | yes | unique, `[a-z][a-z0-9_-]{0,31}`; also the herdr agent name |
| `schedule` | no | cron, 5 fields `m h dom mon dow` or 6 fields with leading seconds. Omit it for a **manual task**: never run by the daemon, only via `cargo run -- run <name>` |
| `prompt` | yes | what the agent does; supports placeholders (below) |
| `cwd` | no | directory the harness starts in (falls back to `defaults.cwd`) |
| `harness` | no | `pi` (default), `claude`, `codex`, ... anything `herdr agent start --kind` accepts |
| `args` | no | replaces `defaults.args` entirely, e.g. `["--model", "claude-sonnet-5-5"]` |
| `runner` | no | `herdr` (workspace + interactive agent, default) or `process` (`harness -p prompt`) |
| `parallel` | no | `true` lets it run alongside other tasks; default is strictly one at a time |
| `exception` | no | natural-language rule evaluated by the local LLM; a SKIP answer skips the run |
| `timeout_seconds` | no | max wall-clock for one run (default 3600) |
| `close_workspace_on_done` | no | close the herdr workspace once the agent settles |
| `fresh_workspace` | no | default `false`: reruns reuse the live `<name>` agent, else the still-open `cron:<name>` workspace, else create one. `true` creates a new `cron:<name>:<MM-DD HH:MM>` workspace and agent on every run |
| `enabled` | no | default `true`; set `false` to keep a task without scheduling it |

Placeholders usable in `prompt` and `exception`: `${date}` (YYYY-MM-DD),
`${time}` (HH:MM), `${datetime}`, `${weekday}`, `${holidays}` (comma-joined
config list), `${task}` (task name).

## Example

```json
{
  "name": "weekly-dep-audit",
  "schedule": "0 8 * * 1",
  "harness": "claude",
  "args": ["--model", "claude-sonnet-5-5"],
  "cwd": "/Users/olivier/dev/frontend",
  "prompt": "Run `npm audit` and summarize actionable findings into audit-${date}.md.",
  "exception": "Skip on holidays. Today is ${weekday} ${date}; holidays: ${holidays}."
}
```

## Manual task example

Same fields, no `schedule`; the user starts it on demand.

```json
{
  "name": "release-notes",
  "harness": "claude",
  "args": ["--model", "claude-sonnet-5-5"],
  "cwd": "/Users/olivier/dev/frontend",
  "prompt": "Draft release notes from the commits since the last tag into release-notes-${date}.md."
}
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
- Prompts are passed as a single argument (no shell), so quotes are safe;
  keep them one JSON string, using `\n` only if the harness tolerates it.
- A rerun is refused while the task's previous agent is still `working`;
  use `fresh_workspace: true` (or `parallel`) if overlapping runs are wanted.
- `crons.json` is committed: keep secrets out of prompts and `crons.example.json` generic.
