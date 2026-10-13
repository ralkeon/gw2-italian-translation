// Statements of /sync. ?1 is a JSON array of [id, hash, en, it, status] rows, ?2 the sender,
// ?3 the time (unix ms). Kept apart so that test.mjs runs the very same SQL on a local SQLite.

const ROW = `json_extract(j.value, '$[0]') AS id, json_extract(j.value, '$[1]') AS hash,
    json_extract(j.value, '$[2]') AS en, json_extract(j.value, '$[3]') AS it,
    json_extract(j.value, '$[4]') AS status`;

/** What a hand correction is about to replace. */
export const SQL_HISTORY = `INSERT INTO history (id, hash, it, status, author, at, replaced_by, replaced_at)
SELECT t.id, t.hash, t.it, t.status, t.author, t.at, ?2, ?3
FROM (SELECT ${ROW} FROM json_each(?1) j) r
JOIN translations t ON t.id = r.id AND t.hash = r.hash
WHERE r.status = 'manual' AND (t.it <> r.it OR t.status <> 'manual')`;

/**
 * New texts, and hand corrections over anything. The scalar subquery does not depend on the row,
 * so SQLite runs it once: every row changed by this statement gets its own seq, above all others.
 */
export const SQL_UPSERT = `INSERT INTO translations (id, hash, en, it, status, author, at, seq)
SELECT r.id, r.hash, r.en, r.it, r.status, ?2, ?3,
       (SELECT coalesce(max(seq), 0) FROM translations) + r.n
FROM (SELECT ${ROW}, j.key + 1 AS n FROM json_each(?1) j) r
WHERE true
ON CONFLICT (id, hash) DO UPDATE SET
    en = excluded.en, it = excluded.it, status = excluded.status, author = excluded.author,
    at = excluded.at, seq = excluded.seq
WHERE excluded.status = 'manual' AND (translations.it <> excluded.it OR translations.status <> 'manual')`;

/** What changed after ?1, oldest first, at most ?2 rows. */
export const SQL_PULL = `SELECT id, hash, en, it, status, author, seq FROM translations
WHERE seq > ?1 ORDER BY seq LIMIT ?2`;
