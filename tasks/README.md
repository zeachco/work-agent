# Tasks

One TOML file per task; the file stem is the task name (`[a-z][a-z0-9_-]{0,31}`).
Only this README is tracked. Examples live in `../examples/tasks/`, the field
reference in the README at the repo root. The daemon reloads when a file here
is added, edited or removed.
