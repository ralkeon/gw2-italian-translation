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
  const row = await env.DB.prepare("SELECT name, banned FROM users WHERE token_hash = ?1")
    .bind(await sha256(token))
    .first();
  if (!row) throw new HttpError(401, "unknown token");
  if (row.banned) throw new HttpError(403, "banned");
  return row.name;
}

/** A row sent by the addon: [id, hash, en, it, status]. Null if malformed. */
function valid(row) {
  if (!Array.isArray(row) || row.length !== 5) return null;
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
  const name = await user(request, env);
  const input = await body(request);
  const since = Number.isSafeInteger(input.since) && input.since > 0 ? input.since : 0;
  const sent = Array.isArray(input.rows) ? input.rows : [];
  if (sent.length > PUSH_MAX) throw new HttpError(413, `at most ${PUSH_MAX} rows`);
  const rows = sent.map(valid).filter(Boolean);

  if (rows.length > 0) {
    const json = JSON.stringify(rows);
    const now = Date.now();
    await env.DB.batch([
      env.DB.prepare(SQL_HISTORY).bind(json, name, now),
      env.DB.prepare(SQL_UPSERT).bind(json, name, now),
    ]);
  }

  const results = await env.DB.prepare(SQL_PULL).bind(since, PULL_MAX).raw();
  const seq = results.length > 0 ? results[results.length - 1][6] : since;
  return reply({ rows: results, seq, more: results.length === PULL_MAX, next_s: NEXT_S, accepted: rows.length });
}
