-- The tables of the old machine-translation pipeline (/translate, the review, the plurals and the
-- consistency jobs), whose code was removed on 2026-11-20. The databases have them and keep them: only
-- created if missing, as before, and read or written by nothing now.

-- /translate: characters translated with the server's keys, per player and day (days since 1970, UTC).
CREATE TABLE IF NOT EXISTS usage (
    day   INTEGER NOT NULL,
    user  TEXT    NOT NULL,          -- users.token_hash
    chars INTEGER NOT NULL,
    PRIMARY KEY (day, user)
);
-- The same, per client address.
CREATE TABLE IF NOT EXISTS usage_ip (
    day   INTEGER NOT NULL,
    ip    TEXT    NOT NULL,
    chars INTEGER NOT NULL,
    PRIMARY KEY (day, ip)
);

-- The consistency job: the groups it has looked at, and their forms then.
CREATE TABLE IF NOT EXISTS consistency_state (
    word      TEXT    PRIMARY KEY,
    signature TEXT    NOT NULL,
    checked   INTEGER NOT NULL
);
-- Every text the job rewrote, to undo it if the model got it wrong.
CREATE TABLE IF NOT EXISTS consistency_log (
    id   INTEGER NOT NULL,
    hash TEXT    NOT NULL,
    word TEXT    NOT NULL,
    old  TEXT    NOT NULL,
    new  TEXT    NOT NULL,
    at   INTEGER NOT NULL
);
-- Whether the model called the word a name (a group worth settling), once for every word.
CREATE TABLE IF NOT EXISTS consistency_words (
    word    TEXT    PRIMARY KEY,
    name    INTEGER NOT NULL,
    checked INTEGER NOT NULL
);

-- The review: the texts (English by hash, Italian) the model has seen.
CREATE TABLE IF NOT EXISTS reviewed (
    hash    TEXT    NOT NULL,
    it      TEXT    NOT NULL,
    verdict TEXT    NOT NULL,     -- ok, fixed (this is the model's fix), replaced, refused
    at      INTEGER NOT NULL,
    PRIMARY KEY (hash, it)
);
-- Every fix, to undo it if needed.
CREATE TABLE IF NOT EXISTS review_log (
    hash TEXT    NOT NULL,
    en   TEXT    NOT NULL,
    old  TEXT    NOT NULL,
    new  TEXT    NOT NULL,
    why  TEXT    NOT NULL,
    rows INTEGER NOT NULL,
    at   INTEGER NOT NULL
);
-- Texts given to the model per day (days since 1970, UTC).
CREATE TABLE IF NOT EXISTS review_usage (
    day   INTEGER PRIMARY KEY,
    texts INTEGER NOT NULL
);

-- The plurals job.
CREATE TABLE IF NOT EXISTS plural_reviewed (
    hash    TEXT    NOT NULL,
    it      TEXT    NOT NULL,
    verdict TEXT    NOT NULL,     -- ok, fixed (the model's fix), replaced, refused
    at      INTEGER NOT NULL,
    PRIMARY KEY (hash, it)
);
CREATE TABLE IF NOT EXISTS plural_log (
    hash TEXT    NOT NULL,
    en   TEXT    NOT NULL,
    old  TEXT    NOT NULL,
    new  TEXT    NOT NULL,
    why  TEXT    NOT NULL,
    rows INTEGER NOT NULL,
    at   INTEGER NOT NULL
);
