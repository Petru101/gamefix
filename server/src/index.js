// gamefix's server: takes bug reports and feedback from gamefix (`gamefix feedback`, the
// right-click menu's "Report a problem") and keeps them in a D1 database for the developer to
// read (reports.sh). Nothing is sent anywhere from here, and no email is involved.

const LIMITS = {
  body: 512 * 1024,
  message: 5000,
  contact: 200,
  game: 200,
  version: 40,
  system: 500,
  report: 200 * 1024,
  last: 100 * 1024,
  record: 100 * 1024,
};
// Reports one install can send in an hour, and all installs together in a day (keeps a flood
// within the free tier; a real person never gets near either).
const PER_INSTALL_HOUR = 5;
const ALL_DAY = 300;

const PRIVACY = `gamefix: what its server keeps

When you report a problem from gamefix, the server keeps what gamefix showed you before you
sent it: your message, the contact you typed (if any), gamefix's version, the game's name,
gamefix's report on the game and what it last said about it, what it changed for the game, and
one line about your system (the Linux distribution, your graphics card's maker, whether Steam's
runtime is there). Your home folder's path, your user name and your Steam account number are
taken out first. It also keeps a random id gamefix made for this install, to stop floods and to
tell reports apart. Nothing else is collected and nothing is sent anywhere.

The server runs on Cloudflare, which sees your IP address like any website does. gamefix's
server doesn't store it.

Reports are read only by gamefix's developer, to fix problems and answer questions.
`;

function reply(status, body) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/** The field if it is a string no longer than `max` characters, else undefined. */
function text(value, max, required = false) {
  if (value === undefined || value === null || value === "") {
    return required ? undefined : null;
  }
  return typeof value === "string" && value.length <= max ? value : undefined;
}

async function feedback(request, env) {
  if (Number(request.headers.get("content-length") || 0) > LIMITS.body) {
    return reply(413, { error: "too big" });
  }
  const raw = await request.text();
  if (raw.length > LIMITS.body) {
    return reply(413, { error: "too big" });
  }
  let body;
  try {
    body = JSON.parse(raw);
  } catch {
    return reply(400, { error: "not JSON" });
  }
  const report = {
    id: typeof body.id === "string" && /^[0-9A-HJKMNP-TV-Z]{4}-[0-9A-HJKMNP-TV-Z]{4}$/.test(body.id) ? body.id : undefined,
    install: typeof body.install === "string" && /^[0-9a-f]{32}$/.test(body.install) ? body.install : undefined,
    version: text(body.version, LIMITS.version, true),
    message: text(typeof body.message === "string" ? body.message.trim() : body.message, LIMITS.message, true),
    contact: text(typeof body.contact === "string" ? body.contact.trim() : body.contact, LIMITS.contact),
    game: text(body.game, LIMITS.game),
    system: text(body.system, LIMITS.system),
    report: text(body.report, LIMITS.report),
    last: text(body.last, LIMITS.last),
    record: text(body.record, LIMITS.record),
  };
  const bad = Object.entries(report).find(([, v]) => v === undefined);
  if (bad) {
    return reply(400, { error: `missing or too long: ${bad[0]}` });
  }
  const [mine, all] = await env.DB.batch([
    env.DB.prepare("SELECT count(*) AS n FROM reports WHERE install = ? AND received > datetime('now', '-1 hour')").bind(report.install),
    env.DB.prepare("SELECT count(*) AS n FROM reports WHERE received > datetime('now', '-1 day')"),
  ]);
  if (mine.results[0].n >= PER_INSTALL_HOUR || all.results[0].n >= ALL_DAY) {
    return reply(429, { error: "too many reports, try again later" });
  }
  const insert = env.DB.prepare(
    "INSERT INTO reports (ref, install, version, message, contact, game, system, report, last, record) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (ref) DO NOTHING",
  ).bind(report.id, report.install, report.version, report.message, report.contact, report.game, report.system, report.report, report.last, report.record);
  const { meta } = await insert.run();
  if (meta.changes === 0) {
    return reply(409, { error: "a report with this id exists" });
  }
  return reply(200, { ok: true, id: report.id });
}

export default {
  async fetch(request, env) {
    const { pathname } = new URL(request.url);
    if (pathname === "/v1/feedback") {
      return request.method === "POST" ? feedback(request, env) : reply(405, { error: "POST only" });
    }
    if (pathname === "/privacy" && request.method === "GET") {
      return new Response(PRIVACY, { headers: { "content-type": "text/plain; charset=utf-8" } });
    }
    return reply(404, { error: "not found" });
  },
};
