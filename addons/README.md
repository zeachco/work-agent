# Addons

Self-contained modules a task can call from any harness. The `claude` harness
has claude.ai MCP connectors (Slack, Atlassian, Google Calendar, ...); the
default `pi` harness on a local model has none. An addon closes that gap: a
small CLI with no package install, run with `bash`/`node`, printing plain text
or JSON the agent can read.

Rules of the folder:

- one directory per addon: `addons/<name>/` with a `README.md` and the entry
  script;
- no network credentials in the repo: tokens live under
  `~/.config/work-agent/<name>/`;
- read-only by default; a write command must be an explicit subcommand the
  prompt has to name;
- stable, machine-readable output (`--json`) plus a human default.

Reference an addon from a prompt by its absolute path, e.g.

```
node /Users/olivier/dev/crons/addons/google-calendar/gcal.mjs today
```

| addon | purpose |
|---|---|
| [google-calendar](google-calendar/README.md) | list calendars and events (today / week / range / search) from Google Calendar |
