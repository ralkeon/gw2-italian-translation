# Global translations database

Cloudflare Worker + D1 that every installation of the addon syncs with (crates/addon/src/sync.rs).
Free plan: 100k requests/day, 100k rows written/day, 5M rows read/day.

## API

- `POST /register {"name"}` -> `{"token"}`: one token per installation (the addon keeps it in
  config.toml). Only its SHA-256 is stored.
- `POST /sync {"since", "rows": [[id, hash, en, it, status]...]}` with `Authorization: Bearer <token>`
  -> `{"rows": [[id, hash, en, it, status, author, seq]...], "seq", "more", "next_s", "accepted"}`.
  Sends up to 200 rows, gets back what changed after `since` (up to 2000; `more` if there is more).

Merge: a `manual` row replaces anything (the old text goes to `history`); an `auto` row is only
stored for a text that has none. `next_s` is the wait the addon keeps between syncs: raise
`NEXT_S` in src/index.js if the free plan runs short.

## Setup

```sh
npm install
npx wrangler login
npx wrangler d1 create gw2it            # copy database_id into wrangler.toml
npx wrangler d1 execute gw2it --remote --file schema.sql
npx wrangler deploy                     # prints the URL: put it in SYNC_URL (crates/addon/src/config.rs)
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
