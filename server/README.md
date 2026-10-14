# Global translations database

The database every installation of the addon syncs with (crates/addon/src/sync.rs), in two
interchangeable versions with the same API, schema (schema.sql) and statements (sql/*.sql):

- **crates/server** (in use): a Rust server on a SQLite file, behind Apache on ifalna:
  https://ifalna.ralkeon.com/gw2it (the addon's default, SYNC_URL in crates/addon/src/config.rs).
- **this folder**: a Cloudflare Worker + D1, https://gw2it-sync.gw2it-sync.workers.dev: a cold
  spare, not kept in sync and not used by the addon unless `[sync] url` points to it (free plan:
  100k requests/day, 100k rows written/day, 5M rows read/day).

A database moves from one to the other with its users and seq, so the addons carry on where they
were (no new token, no new download): `wrangler d1 export` → `sqlite3 gw2it.db < dump.sql`.

## API

- `POST /register {"name"}` -> `{"token"}`: one token per installation (the addon keeps it in
  config.toml). Only its SHA-256 is stored.
- `POST /sync {"since", "rows": [[id, hash, en, it, status]...]}` with `Authorization: Bearer <token>`
  -> `{"rows": [[id, hash, en, it, status, author, seq]...], "seq", "more", "next_s", "accepted"}`.
  Sends up to 200 rows, gets back what changed after `since` (up to 2000; `more` if there is more).

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

Data in `~/gw2it-sync/data/gw2it.db`; logs: `journalctl -u gw2it-sync`.

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
GW2IT_SYNC_URL=http://localhost:8787 cargo test -p gw2it-addon -- --ignored   # the addon against it
```

## Maintenance

```sh
npx wrangler d1 execute gw2it --remote --command "SELECT author, status, count(*) FROM translations GROUP BY 1, 2"
npx wrangler d1 execute gw2it --remote --command "UPDATE users SET banned = 1 WHERE name = '...'"
```
