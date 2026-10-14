-- New texts, and hand corrections over anything. ?1 ?2 ?3 as in history.sql.
-- The scalar subquery does not depend on the row, so SQLite runs it once: every row changed by
-- this statement gets its own seq, above all others.
INSERT INTO translations (id, hash, en, it, status, author, at, seq)
SELECT r.id, r.hash, r.en, r.it, r.status, ?2, ?3,
       (SELECT coalesce(max(seq), 0) FROM translations) + r.n
FROM (SELECT json_extract(j.value, '$[0]') AS id, json_extract(j.value, '$[1]') AS hash,
    json_extract(j.value, '$[2]') AS en, json_extract(j.value, '$[3]') AS it,
    json_extract(j.value, '$[4]') AS status, j.key + 1 AS n FROM json_each(?1) j) r
WHERE true
ON CONFLICT (id, hash) DO UPDATE SET
    en = excluded.en, it = excluded.it, status = excluded.status, author = excluded.author,
    at = excluded.at, seq = excluded.seq
WHERE excluded.status = 'manual' AND (translations.it <> excluded.it OR translations.status <> 'manual');
