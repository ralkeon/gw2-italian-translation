-- Global translations database (Cloudflare D1).

-- One row per text: the translation in use. `seq` grows with every change, so that a client asks
-- only for what changed since its last sync.
CREATE TABLE IF NOT EXISTS translations (
    id     INTEGER NOT NULL,          -- string ID
    hash   TEXT    NOT NULL,          -- XXH3-64 of the English, 16 hex digits
    en     TEXT    NOT NULL,
    it     TEXT    NOT NULL,
    status TEXT    NOT NULL,          -- 'auto' (machine) or 'manual' (hand correction)
    author TEXT    NOT NULL,          -- who sent it
    at     INTEGER NOT NULL,          -- unix ms, when the server received it
    seq    INTEGER NOT NULL,
    -- Whose word it is, from the least to the most reliable (sql/upsert.sql): 0 machine translation
    -- sent by an addon, 1 machine translation made by the server itself, 2 hand correction of a
    -- contributor, 3 of the owner. A row only replaces a lower one; two hand corrections: the last.
    trust  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (id, hash)
);
CREATE UNIQUE INDEX IF NOT EXISTS translations_seq ON translations (seq);

-- Translations replaced by a hand correction, to restore one if needed.
CREATE TABLE IF NOT EXISTS history (
    id     INTEGER NOT NULL,
    hash   TEXT    NOT NULL,
    it     TEXT    NOT NULL,
    status TEXT    NOT NULL,
    author TEXT    NOT NULL,
    at     INTEGER NOT NULL,
    replaced_by TEXT NOT NULL,
    replaced_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS history_key ON history (id, hash);

-- One row per installation. Only the SHA-256 of the token is kept.
CREATE TABLE IF NOT EXISTS users (
    token_hash TEXT PRIMARY KEY,
    name       TEXT    NOT NULL,
    created    INTEGER NOT NULL,
    banned     INTEGER NOT NULL DEFAULT 0,
    -- 0 nobody special, 1 may send hand corrections (the others' 'manual' rows are taken as 'auto'),
    -- 2 the owner: the owner's hand corrections beat the contributors'. Set by hand (server/README.md).
    -- Older databases: ALTER TABLE users ADD COLUMN contributor ...
    contributor INTEGER NOT NULL DEFAULT 0
);
