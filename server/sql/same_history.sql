-- What same.sql is about to replace (the same CTEs; keep them in step).
WITH touched AS (
    SELECT DISTINCT json_extract(j.value, '$[1]') AS hash, json_extract(j.value, '$[2]') AS en FROM json_each(?1) j
), counted AS (
    SELECT t.hash, t.en, t.it, t.status, t.author, t.trust, t.seq,
           count(*) OVER (PARTITION BY t.hash, t.en, t.it, t.trust >= 2) AS rows_with_it
    FROM translations t JOIN touched USING (hash, en)
), best AS (
    SELECT * FROM (
        SELECT c.*, row_number() OVER (PARTITION BY c.hash, c.en ORDER BY c.trust >= 2 DESC,
                   CASE WHEN c.trust >= 2 THEN c.rows_with_it ELSE 0 END DESC, c.trust DESC,
                   CASE WHEN c.trust >= 2 THEN -c.seq ELSE c.seq END) AS r
        FROM counted c
    ) WHERE r = 1
)
INSERT INTO history (id, hash, it, status, author, at, replaced_by, replaced_at)
SELECT t.id, t.hash, t.it, t.status, t.author, t.at, b.author, ?3
FROM translations t JOIN best b ON b.hash = t.hash AND b.en = t.en
WHERE t.trust < 2 AND t.trust < b.trust AND t.it <> b.it;
