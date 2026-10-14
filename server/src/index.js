// Italian Translation: global translations database.
//
// Every installation of the addon calls POST /sync every few minutes: it sends the translations
// made since its last call (by DeepL or by hand) and gets back those that changed on the server
// since its last call. One request does both, to stay well inside the free plan.
//
// Merge rules, the same as in the addon:
// - a hand correction ('manual') replaces anything, and the replaced text goes to `history`;
// - a machine translation ('auto') is only stored for a text that has none: the first one stays,
//   so every player sees the same text.
//
// Every write is a single statement over a JSON array (json_each): D1 counts statements per
// request, and a statement is atomic, so `seq` is never handed out twice.
//
// This Worker is the spare of the main server (crates/server on ifalna): the addon comes here only
// when the main server does not answer. Once a day the main server takes back what was written
// here (POST /replica/export) and sends its own changes (POST /replica/import). The seq handed
// out here start at SEQ_FLOOR, so they never mix with the main server's.

// The statements are shared with the self-hosted server (crates/server) and test.mjs.
import SQL_HISTORY from "../sql/history.sql";
import SQL_UPSERT from "../sql/upsert.sql";
import SQL_PULL from "../sql/pull.sql";

/** Rows accepted in one /sync: the addon sends at most this many. */
const PUSH_MAX = 200;
/** Rows returned by one /sync: with more, `more` is true and the addon calls again at once. */
const PULL_MAX = 2000;
/** Seconds the addon waits before the next /sync: raised here if the free plan runs short. */
const NEXT_S = 120;
const TEXT_MAX = 4000;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    try {
      if (request.method === "POST" && url.pathname === "/register") return await register(request, env);
      if (request.method === "POST" && url.pathname === "/sync") return await sync(request, env);
      if (request.method === "POST" && url.pathname === "/replica/export") return await replicaExport(request, env);
      if (request.method === "POST" && url.pathname === "/replica/import") return await replicaImport(request, env);
      if (request.method === "GET" && url.pathname === "/") return reply({ service: "gw2it-sync" });
      return reply({ error: "not found" }, 404);
    } catch (e) {
      if (e instanceof HttpError) return reply({ error: e.message }, e.status);
      console.error(e);
      return reply({ error: "internal error" }, 500);
    }
  },
};

class HttpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

function reply(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

async function limit(env, key) {
  const { success } = await env.LIMITER.limit({ key });
  if (!success) throw new HttpError(429, "too many requests");
}

async function body(request) {
  try {
    return await request.json();
  } catch {
    throw new HttpError(400, "body is not JSON");
  }
}

async function sha256(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** POST /register {name} -> {token}: one token per installation, kept in its config.toml. */
async function register(request, env) {
  await limit(env, "register:" + (request.headers.get("cf-connecting-ip") ?? "?"));
  const { name } = await body(request);
  const clean = typeof name === "string" ? name.replace(/[\u0000-\u001f]/g, "").trim().slice(0, 32) : "";
  const token = [...crypto.getRandomValues(new Uint8Array(24))].map((b) => b.toString(16).padStart(2, "0")).join("");
  await env.DB.prepare("INSERT INTO users (token_hash, name, created) VALUES (?1, ?2, ?3)")
    .bind(await sha256(token), clean || "anonimo", Date.now())
    .run();
  return reply({ token });
}

async function user(request, env) {
  const auth = request.headers.get("authorization") ?? "";
  const token = auth.startsWith("Bearer ") ? auth.slice(7).trim() : "";
  if (!token) throw new HttpError(401, "missing token");
  await limit(env, "sync:" + token);
  const row = await env.DB.prepare("SELECT name, banned, contributor FROM users WHERE token_hash = ?1")
    .bind(await sha256(token))
    .first();
  if (!row) throw new HttpError(401, "unknown token");
  if (row.banned) throw new HttpError(403, "banned");
  return row;
}

/** A row sent by the addon: [id, hash, en, it, status] (+ proof). The first five, null if malformed. */
function valid(row) {
  // 7 elements: the addon's proof that the server made the translation, which only the main server
  // checks (crates/server).
  if (!Array.isArray(row) || (row.length !== 5 && row.length !== 7)) return null;
  const [id, hash, en, it, status] = row;
  if (!Number.isInteger(id) || id < 0 || id > 0xffffffff) return null;
  if (typeof hash !== "string" || !/^[0-9a-f]{16}$/.test(hash)) return null;
  if (typeof en !== "string" || en.length > TEXT_MAX) return null;
  if (typeof it !== "string" || !it.trim() || it.length > TEXT_MAX) return null;
  if (status !== "auto" && status !== "manual") return null;
  return [id, hash, en, it, status];
}

/**
 * POST /sync {since, rows: [[id, hash, en, it, status]...]}
 *   -> {rows: [[id, hash, en, it, status, author, seq]...], seq, more, next_s, accepted}
 */
async function sync(request, env) {
  const { name, contributor: level } = await user(request, env);
  const input = await body(request);
  const since = Number.isSafeInteger(input.since) && input.since > 0 ? input.since : 0;
  const sent = Array.isArray(input.rows) ? input.rows : [];
  if (sent.length > PUSH_MAX) throw new HttpError(413, `at most ${PUSH_MAX} rows`);
  const rows = sent.map(valid).filter(Boolean);
  // Anyone can write contributor.json: only the contributors' hand corrections count as such.
  // The sixth element is the author (the sender), the seventh the trust: see schema.sql.
  for (const row of rows) {
    let trust = 0;
    if (row[4] === "manual" && level >= 1) trust = level >= 2 ? 3 : 2;
    else row[4] = "auto";
    row.push(null, trust);
  }

  if (rows.length > 0) {
    const json = JSON.stringify(rows);
    const now = Date.now();
    await env.DB.batch([
      env.DB.prepare(SQL_HISTORY).bind(json, name, now),
      env.DB.prepare(SQL_UPSERT).bind(json, name, now, floor(env)),
    ]);
  }

  const results = await env.DB.prepare(SQL_PULL).bind(since, PULL_MAX).raw();
  const seq = results.length > 0 ? results[results.length - 1][6] : since;
  return reply({ rows: results, seq, more: results.length === PULL_MAX, next_s: NEXT_S, accepted: rows.length });
}

function floor(env) {
  return Number(env.SEQ_FLOOR ?? 0);
}

/** The main server's calls: with the key set by `wrangler secret put REPLICA_KEY`. */
async function replica(request, env) {
  const key = request.headers.get("x-replica-key") ?? "";
  if (!env.REPLICA_KEY || key !== env.REPLICA_KEY) throw new HttpError(404, "not found");
  return await body(request);
}

/** Rows of one replica call: enough for a day of fallback, small enough for one D1 statement. */
const REPLICA_MAX = 300;

/**
 * POST /replica/export {after, since} -> {translations, users, history}
 *   translations written here (seq >= SEQ_FLOOR, after `after`, oldest first, up to REPLICA_MAX):
 *     [id, hash, en, it, status, author, at, seq, trust]
 *   users registered here since `since` (unix ms): [token_hash, name, created, banned, contributor]
 *   history (only what was written here: the main server's is not copied): every row.
 */
async function replicaExport(request, env) {
  const input = await replica(request, env);
  const after = Math.max(Number.isSafeInteger(input.after) ? input.after : 0, floor(env) - 1);
  const since = Number.isSafeInteger(input.since) ? input.since : 0;
  const [translations, users, history] = await env.DB.batch([
    env.DB.prepare(
      "SELECT id, hash, en, it, status, author, at, seq, trust FROM translations WHERE seq > ?1 ORDER BY seq LIMIT ?2",
    ).bind(after, REPLICA_MAX),
    env.DB.prepare("SELECT token_hash, name, created, banned, contributor FROM users WHERE created >= ?1").bind(since),
    env.DB.prepare("SELECT id, hash, it, status, author, at, replaced_by, replaced_at FROM history"),
  ]);
  const rows = (r) => r.results.map((o) => Object.values(o));
  return reply({ translations: rows(translations), users: rows(users), history: rows(history) });
}

/**
 * POST /replica/import {clear, translations: [[id, hash, en, it, status, author, at, seq, trust]...],
 *   users: [[token_hash, name, created, banned, contributor]...]}
 *   The main server's rows, as they are there. `clear` (first call of a run): drop what was written
 *   here, the main server has taken it back.
 */
async function replicaImport(request, env) {
  const input = await replica(request, env);
  const translations = Array.isArray(input.translations) ? input.translations : [];
  const users = Array.isArray(input.users) ? input.users : [];
  if (translations.length > REPLICA_MAX || users.length > 5000) throw new HttpError(413, "too many rows");
  const statements = [];
  if (input.clear === true) {
    statements.push(env.DB.prepare("DELETE FROM translations WHERE seq >= ?1").bind(floor(env)));
    statements.push(env.DB.prepare("DELETE FROM history"));
  }
  if (translations.length > 0) {
    statements.push(
      env.DB.prepare(
        `INSERT OR REPLACE INTO translations (id, hash, en, it, status, author, at, seq, trust)
         SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]'), json_extract(value, '$[2]'),
                json_extract(value, '$[3]'), json_extract(value, '$[4]'), json_extract(value, '$[5]'),
                json_extract(value, '$[6]'), json_extract(value, '$[7]'), coalesce(json_extract(value, '$[8]'), 0)
         FROM json_each(?1)`,
      ).bind(JSON.stringify(translations)),
    );
  }
  if (users.length > 0) {
    statements.push(
      env.DB.prepare(
        `INSERT OR REPLACE INTO users (token_hash, name, created, banned, contributor)
         SELECT json_extract(value, '$[0]'), json_extract(value, '$[1]'), json_extract(value, '$[2]'),
                json_extract(value, '$[3]'), coalesce(json_extract(value, '$[4]'), 0)
         FROM json_each(?1)`,
      ).bind(JSON.stringify(users)),
    );
  }
  if (statements.length > 0) await env.DB.batch(statements);
  return reply({ translations: translations.length, users: users.length });
}
