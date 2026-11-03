#!/usr/bin/env node
// google-calendar addon: read-only Google Calendar CLI with no dependencies.
// See README.md in this directory for setup and commands.

import { createServer } from "node:http";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { randomBytes, createHash } from "node:crypto";
import { homedir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";

const CONFIG_DIR = join(homedir(), ".config", "work-agent", "google-calendar");
const CLIENT_FILE = process.env.GCAL_CLIENT_FILE ?? join(CONFIG_DIR, "client.json");
const TOKEN_FILE = process.env.GCAL_TOKEN_FILE ?? join(CONFIG_DIR, "token.json");
const SCOPE = "https://www.googleapis.com/auth/calendar.readonly";
const API = "https://www.googleapis.com/calendar/v3";
const DEFAULT_TZ = "America/Toronto";

// ---------- CLI parsing ----------

function parseArgs(argv) {
  const flags = {};
  const positional = [];
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      const key = a.slice(2);
      const next = argv[i + 1];
      if (next === undefined || next.startsWith("--")) flags[key] = true;
      else {
        flags[key] = next;
        i++;
      }
    } else positional.push(a);
  }
  return { flags, positional };
}

function usage() {
  console.error(`usage: gcal.mjs <command> [options]

  auth                                  interactive consent, stores the refresh token
  calendars                             list calendars
  today     [--calendar ID]             events today
  week      [--calendar ID]             events Monday..Sunday this week
  events    --from DATE --to DATE [--calendar ID] [--query TEXT]
  search    TEXT [--days N]             events matching TEXT in the next N days (60)

  common:   --json   --tz ZONE (default ${DEFAULT_TZ})   --calendar primary|all|ID`);
  process.exit(1);
}

// ---------- auth ----------

async function readJson(path) {
  return JSON.parse(await readFile(path, "utf8"));
}

async function loadClient() {
  let raw;
  try {
    raw = await readJson(CLIENT_FILE);
  } catch {
    fail(`missing OAuth client file ${CLIENT_FILE}; see README.md (setup)`, 2);
  }
  const c = raw.installed ?? raw.web ?? raw;
  if (!c.client_id || !c.client_secret) fail(`${CLIENT_FILE} has no client_id/client_secret`, 2);
  return { id: c.client_id, secret: c.client_secret };
}

async function auth() {
  const client = await loadClient();
  const verifier = randomBytes(32).toString("base64url");
  const challenge = createHash("sha256").update(verifier).digest("base64url");
  const state = randomBytes(8).toString("hex");

  const { code, redirect } = await new Promise((resolve, reject) => {
    let redirect;
    const server = createServer((req, res) => {
      const url = new URL(req.url, "http://127.0.0.1");
      if (url.pathname !== "/") return res.end();
      res.setHeader("content-type", "text/plain");
      if (url.searchParams.get("state") !== state) {
        res.end("state mismatch; retry gcal.mjs auth");
        return;
      }
      const err = url.searchParams.get("error");
      if (err) {
        res.end(`error: ${err}`);
        server.close();
        reject(new Error(err));
        return;
      }
      res.end("Authenticated. You can close this tab.");
      server.close();
      resolve({ code: url.searchParams.get("code"), redirect });
    });
    server.listen(0, "127.0.0.1", () => {
      redirect = `http://127.0.0.1:${server.address().port}/`;
      const u = new URL("https://accounts.google.com/o/oauth2/v2/auth");
      u.search = new URLSearchParams({
        client_id: client.id,
        redirect_uri: redirect,
        response_type: "code",
        scope: SCOPE,
        access_type: "offline",
        prompt: "consent",
        code_challenge: challenge,
        code_challenge_method: "S256",
        state,
      });
      console.error(`Open this URL to authorize:\n${u}`);
      spawn("open", [u.toString()], { stdio: "ignore" }).on("error", () => {});
    });
  });

  const token = await fetchJson("https://oauth2.googleapis.com/token", {
    client_id: client.id,
    client_secret: client.secret,
    code,
    code_verifier: verifier,
    grant_type: "authorization_code",
    redirect_uri: redirect,
  });
  if (!token.refresh_token) fail("Google returned no refresh_token; revoke the app at myaccount.google.com/permissions and retry", 1);
  await mkdir(CONFIG_DIR, { recursive: true });
  await writeFile(
    TOKEN_FILE,
    JSON.stringify({ refresh_token: token.refresh_token, scope: SCOPE, created: new Date().toISOString() }, null, 2) + "\n",
    { mode: 0o600 },
  );
  console.error(`token saved to ${TOKEN_FILE}`);
}

async function fetchJson(url, form) {
  const res = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams(form),
  });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) fail(`${url}: ${body.error ?? res.status} ${body.error_description ?? ""}`.trim(), 1);
  return body;
}

async function accessToken() {
  const client = await loadClient();
  let saved;
  try {
    saved = await readJson(TOKEN_FILE);
  } catch {
    fail(`not authenticated: run \`node ${process.argv[1]} auth\``, 2);
  }
  const t = await fetchJson("https://oauth2.googleapis.com/token", {
    client_id: client.id,
    client_secret: client.secret,
    refresh_token: saved.refresh_token,
    grant_type: "refresh_token",
  });
  return t.access_token;
}

// ---------- API ----------

async function api(token, path, params = {}) {
  const u = new URL(`${API}${path}`);
  for (const [k, v] of Object.entries(params)) if (v !== undefined) u.searchParams.set(k, v);
  const res = await fetch(u, { headers: { authorization: `Bearer ${token}` } });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) fail(`${path}: ${res.status} ${body.error?.message ?? ""}`.trim(), res.status === 401 ? 2 : 1);
  return body;
}

async function listCalendars(token) {
  const out = [];
  let pageToken;
  do {
    const page = await api(token, "/users/me/calendarList", { pageToken, minAccessRole: "reader" });
    out.push(...(page.items ?? []));
    pageToken = page.nextPageToken;
  } while (pageToken);
  return out.map((c) => ({ id: c.id, summary: c.summary, primary: !!c.primary, role: c.accessRole, timeZone: c.timeZone }));
}

async function listEvents(token, calendarId, { timeMin, timeMax, q }) {
  const out = [];
  let pageToken;
  do {
    const page = await api(token, `/calendars/${encodeURIComponent(calendarId)}/events`, {
      timeMin,
      timeMax,
      q,
      singleEvents: "true",
      orderBy: "startTime",
      maxResults: "250",
      pageToken,
    });
    out.push(...(page.items ?? []));
    pageToken = page.nextPageToken;
  } while (pageToken);
  return out.filter((e) => e.status !== "cancelled").map((e) => normalize(e, calendarId));
}

function normalize(e, calendarId) {
  const allDay = !!e.start?.date;
  return {
    id: e.id,
    calendar: calendarId,
    start: e.start?.dateTime ?? e.start?.date,
    end: e.end?.dateTime ?? e.end?.date,
    allDay,
    summary: e.summary ?? "(no title)",
    location: e.location ?? null,
    description: e.description ?? null,
    organizer: e.organizer?.email ?? null,
    attendees: (e.attendees ?? []).map((a) => ({ email: a.email, name: a.displayName ?? null, response: a.responseStatus, self: !!a.self })),
    myResponse: (e.attendees ?? []).find((a) => a.self)?.responseStatus ?? null,
    hangoutLink: e.hangoutLink ?? null,
    htmlLink: e.htmlLink ?? null,
    status: e.status,
  };
}

async function eventsAcross(token, calendarFlag, range) {
  const ids = calendarFlag === "all" ? (await listCalendars(token)).map((c) => c.id) : [calendarFlag ?? "primary"];
  const all = (await Promise.all(ids.map((id) => listEvents(token, id, range)))).flat();
  const seen = new Set();
  return all
    .filter((e) => (seen.has(e.id) ? false : seen.add(e.id)))
    .sort((a, b) => String(a.start).localeCompare(String(b.start)));
}

// ---------- dates ----------

// Local-date arithmetic in the requested zone, returned as RFC3339 instants.
function zonedParts(date, tz) {
  const f = new Intl.DateTimeFormat("en-CA", { timeZone: tz, year: "numeric", month: "2-digit", day: "2-digit", weekday: "short" });
  const p = Object.fromEntries(f.formatToParts(date).map((x) => [x.type, x.value]));
  return { ymd: `${p.year}-${p.month}-${p.day}`, weekday: p.weekday };
}

function offsetFor(ymd, tz) {
  // Offset of local midnight on `ymd` in `tz`, as "+HH:MM".
  const guess = new Date(`${ymd}T00:00:00Z`);
  const f = new Intl.DateTimeFormat("en-US", { timeZone: tz, timeZoneName: "longOffset" });
  const name = f.formatToParts(guess).find((x) => x.type === "timeZoneName").value; // "GMT-05:00" or "GMT"
  const m = /GMT([+-]\d{2}):?(\d{2})?/.exec(name);
  return m ? `${m[1]}:${m[2] ?? "00"}` : "+00:00";
}

function dayStart(ymd, tz) {
  return `${ymd}T00:00:00${offsetFor(ymd, tz)}`;
}

function addDays(ymd, n) {
  const d = new Date(`${ymd}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + n);
  return d.toISOString().slice(0, 10);
}

function rangeFor(cmd, flags, tz) {
  const today = zonedParts(new Date(), tz).ymd;
  if (cmd === "today") return { timeMin: dayStart(today, tz), timeMax: dayStart(addDays(today, 1), tz) };
  if (cmd === "week") {
    const dow = new Date(`${today}T00:00:00Z`).getUTCDay(); // 0 Sunday
    const monday = addDays(today, dow === 0 ? -6 : 1 - dow);
    return { timeMin: dayStart(monday, tz), timeMax: dayStart(addDays(monday, 7), tz) };
  }
  if (cmd === "events") {
    if (!flags.from || !flags.to) usage();
    return { timeMin: dayStart(flags.from, tz), timeMax: dayStart(addDays(flags.to, 1), tz), q: flags.query };
  }
  if (cmd === "search") {
    const days = Number(flags.days ?? 60);
    return { timeMin: dayStart(today, tz), timeMax: dayStart(addDays(today, days), tz) };
  }
  usage();
}

// ---------- output ----------

function fmtTime(iso, tz) {
  return new Intl.DateTimeFormat("en-CA", { timeZone: tz, hour: "2-digit", minute: "2-digit", hour12: false }).format(new Date(iso));
}

function printEvents(events, tz) {
  if (events.length === 0) return console.log("no events");
  let day = "";
  for (const e of events) {
    const d = e.allDay ? e.start : zonedParts(new Date(e.start), tz).ymd;
    if (d !== day) {
      day = d;
      console.log(`\n${zonedParts(new Date(`${d}T12:00:00Z`), tz).weekday} ${d}`);
    }
    const when = e.allDay ? "all day     " : `${fmtTime(e.start, tz)}-${fmtTime(e.end, tz)} `;
    const who = e.attendees.length ? ` (${e.attendees.length} attendees)` : "";
    const rsvp = e.myResponse && e.myResponse !== "accepted" ? ` [${e.myResponse}]` : "";
    console.log(`  ${when}${e.summary}${who}${rsvp}${e.location ? ` @ ${e.location}` : ""}`);
  }
}

function fail(msg, code) {
  console.error(`gcal: ${msg}`);
  process.exit(code);
}

// ---------- main ----------

const { flags, positional } = parseArgs(process.argv.slice(2));
const cmd = positional[0];
const tz = flags.tz ?? DEFAULT_TZ;
if (!cmd) usage();

if (cmd === "auth") {
  await auth();
} else if (cmd === "calendars") {
  const cals = await listCalendars(await accessToken());
  if (flags.json) console.log(JSON.stringify(cals, null, 2));
  else for (const c of cals) console.log(`${c.primary ? "*" : " "} ${c.id}\t${c.summary} (${c.role})`);
} else if (["today", "week", "events", "search"].includes(cmd)) {
  const range = rangeFor(cmd, flags, tz);
  if (cmd === "search") {
    if (!positional[1]) usage();
    range.q = positional[1];
  }
  const events = await eventsAcross(await accessToken(), flags.calendar, range);
  if (flags.json) console.log(JSON.stringify(events, null, 2));
  else printEvents(events, tz);
} else usage();
