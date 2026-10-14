-- New texts, and hand corrections over anything. ?1 ?2 ?3 as in history.sql; ?4 the lowest seq
-- this server hands out (0 on the main server, 10^12 on the spare: see replicate in crates/server).
-- The scalar subquery does not depend on the row, so SQLite runs it once: every row changed by
-- this statement gets its own seq, above all others.
INSERT INTO translations (id, hash, en, it, status, author, at, seq)
SELECT r.id, r.hash, r.en, r.it, r.status, coalesce(r.author, ?2), ?3,
       max((SELECT coalesce(max(seq), 0) FROM translations), ?4) + r.n
FROM (SELECT json_extract(j.value, '$[0]') AS id, json_extract(j.value, '$[1]') AS hash,
    json_extract(j.value, '$[2]') AS en, json_extract(j.value, '$[3]') AS it,
    json_extract(j.value, '$[4]') AS status, json_extract(j.value, '$[5]') AS author,
    j.key + 1 AS n FROM json_each(?1) j) r
WHERE true
ON CONFLICT (id, hash) DO UPDATE SET
    en = excluded.en, it = excluded.it, status = excluded.status, author = excluded.author,
    at = excluded.at, seq = excluded.seq
WHERE excluded.status = 'manual' AND (translations.it <> excluded.it OR translations.status <> 'manual');
