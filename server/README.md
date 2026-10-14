# Global translations database

The database every installation of the addon syncs with (crates/addon/src/sync.rs), in two
interchangeable versions with the same API, schema (schema.sql) and statements (sql/*.sql):

- **crates/server** (in use): a Rust server on a SQLite file, behind Apache on ifalna:
  https://ifalna.ralkeon.com/gw2it (the addon's default, SYNC_URL in crates/addon/src/config.rs).
- **this folder**: a Cloudflare Worker + D1, https://gw2it-sync.gw2it-sync.workers.dev, the
  spare (free plan: 100k requests/day, 100k rows written/day, 5M rows read/day).

## Spare and fallback

- The addon tries the main server first at every sync; when it gets no answer (or a 5xx) it syncs
  with the spare instead, with its own token there (`[sync] fallback_token`).
- Every night at 04:17 cron on ifalna runs `gw2it-server replicate`
  (crates/server/src/replicate.rs): it takes back what was written on the spare (rows with
  seq >= `SEQ_FLOOR` = 10^12, new users, history) and merges it with the usual rules, then sends
  the spare what changed on the main server. The spare then holds the main server's rows with its
  seq, so an addon that falls back carries on from the seq it had.
- `POST /replica/export` and `POST /replica/import` need `X-Replica-Key`: the Worker secret
  `REPLICA_KEY` (`wrangler secret put REPLICA_KEY`), the same as ifalna's
  `~/gw2it-sync/replica.key`. Log: `~/gw2it-sync/replicate.log`.
- A whole database still moves from one to the other with its users and seq:
  `wrangler d1 export` → `sqlite3 gw2it.db < dump.sql`.

## API

- `POST /register {"name"}` -> `{"token"}`: one token per installation (the addon keeps it in
  config.toml). Only its SHA-256 is stored.
- `POST /sync {"since", "rows": [[id, hash, en, it, status]...]}` with `Authorization: Bearer <token>`
  -> `{"rows": [[id, hash, en, it, status, author, seq]...], "seq", "more", "next_s", "accepted"}`.
  Sends up to 200 rows, gets back what changed after `since` (up to 2000; `more` if there is more).

- `POST /translate {"texts": [xml...], "neighbours"}` with the same token -> `{"translations": [xml...]}`
  (crates/server only, with `--translators translators.json`): the server's translators, for players
  without keys: DeepL, then the next ones while it is out of quota or down. Up to 50 texts; 30,000 characters per player per day (table `usage`); errors keep DeepL's
  codes (456 quota or daily limit, 429, 400). On the spare it is missing: the addon waits for the
  main server.

Only contributors (`users.contributor = 1`, set by hand) send `manual` rows: anyone can turn the
addon's editor on, so the others' are stored as `auto`.

Merge: a `manual` row replaces anything (the old text goes to `history`); an `auto` row is only
stored for a text that has none. `next_s` is the wait the addon keeps between syncs: raise
`NEXT_S` in src/index.js if the free plan runs short.

## Self-hosted (crates/server)

```sh
# on the server, with rustup in ~/.cargo: the sources of crates/server and server/{schema.sql,sql}
cargo build --release -p gw2it-server
cp target/release/gw2it-server ~/gw2it-sync/
sudo sh ~/gw2it-sync/install-root.sh   # crates/server/deploy: systemd service + Apache proxy on /gw2it/
```

Data in `~/gw2it-sync/data/gw2it.db`; logs: `journalctl -u gw2it-sync`. The translators of
/translate, in order, with their keys: `~/gw2it-sync/translators.json` (mode 600), e.g.

```json
[
  {"type": "deepl", "key": "...:fx"},
  {"type": "gemini", "key": "...", "model": "gemini-3.1-flash-lite,gemini-3.6-flash"},
  {"type": "azure", "key": "...", "region": "italynorth"},
  {"type": "openai", "name": "Groq", "url": "https://api.groq.com/openai/v1", "key": "...", "model": "openai/gpt-oss-120b"}
]
```

## Cloudflare setup

```sh
npm install
npx wrangler login
npx wrangler d1 create gw2it            # copy database_id into wrangler.toml
npx wrangler d1 execute gw2it --remote --file schema.sql
npx wrangler deploy                     # prints the URL
```

## Tests

```sh
npm test                                # the SQL of /sync on a local SQLite
npx wrangler d1 execute gw2it --local --file schema.sql && npx wrangler dev
GW2IT_SYNC_URL=http://localhost:8787 cargo test -p gw2it-addon against_server -- --ignored   # the addon against it
# fallback and nightly copy end to end: see the comment of sync::tests::failover
```

## Maintenance

```sh
npx wrangler d1 execute gw2it --remote --command "SELECT author, status, count(*) FROM translations GROUP BY 1, 2"
npx wrangler d1 execute gw2it --remote --command "UPDATE users SET banned = 1 WHERE name = '...'"
# a contributor (on ifalna; the spare gets it with the nightly copy)
ssh ifalna "sqlite3 ~/gw2it-sync/data/gw2it.db \"UPDATE users SET contributor = 1 WHERE token_hash = '<SHA-256 of the token in their config.toml>'\""
```
