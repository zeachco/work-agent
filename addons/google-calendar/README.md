# google-calendar addon

Read events from Google Calendar with a dependency-free Node script
(Node 18+). Tokens are kept in `~/.config/work-agent/google-calendar/`.

## One-time setup

1. In Google Cloud console create an OAuth client of type **Desktop app**
   (APIs & Services → Credentials) and enable the **Google Calendar API** on
   that project.
2. Download the client JSON and save it as
   `~/.config/work-agent/google-calendar/client.json`
   (or point `GCAL_CLIENT_FILE` at it).
3. Run `node gcal.mjs auth`. A browser opens, you consent with the stay22
   account, and the refresh token is stored next to the client file.

The scope requested is `calendar.readonly`. Nothing in this addon writes to
the calendar.

## Commands

```
node gcal.mjs auth                      # interactive consent, stores the token
node gcal.mjs calendars                 # calendars the account can see
node gcal.mjs today   [--calendar ID]   # events today (local time)
node gcal.mjs week    [--calendar ID]   # Monday to Sunday of the current week
node gcal.mjs events  --from 2026-11-03 --to 2026-11-10 [--calendar ID] [--query text]
node gcal.mjs search  "Engineering Guild" [--days 60]
```

Every command accepts `--json` for one JSON array of events
(`id, start, end, allDay, summary, location, description, attendees, hangoutLink, htmlLink, status, organizer`)
and `--tz` to override the display time zone (default `America/Toronto`).
`--calendar` defaults to `primary`; pass `--calendar all` to merge every
calendar the account can read.

Exit codes: `0` ok, `2` not authenticated (run `auth`), `1` any other error.

## Use in a task prompt

```
Run `node /Users/olivier/dev/crons/addons/google-calendar/gcal.mjs week --json`
and use the output as the list of my meetings this week.
```
