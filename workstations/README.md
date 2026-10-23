# Workstations

One directory per task, created by the daemon on first run and never committed
(only this README is tracked). Layout:

```
workstations/<task_name>/memory.md   # notes the agent carries from run to run
workstations/<task_name>/...         # temp files and reusable artifacts, e.g. say-hello/format.py
```

Prompts can reference the directory as `${workstation}`. The daemon also prepends
a note to every prompt pointing the agent at it. Delete a directory to reset a
task's memory.
