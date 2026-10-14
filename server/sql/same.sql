-- The same English under other IDs (ten "Torch" items): after upsert.sql, the machine translations
-- (trust 0 and 1) of the English the rows ?1 touched take the best translation of that English, if
-- it is stronger. Best: a hand correction (2 and 3) if there is one, the text most of them have
-- (the general one: a hand correction goes to all the machine translations, so a later one that
-- differs is for its own context only), then the highest trust, then the last; with no hand
-- correction, the highest trust, then the first, as in upsert.sql. A hand correction is never
-- replaced here, whoever made the other one.
-- ?1 ?2 ?3 ?4 as in upsert.sql (?2 unused); same_history.sql runs first, with the same CTEs.
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
), targets AS (
    SELECT t.id, t.hash, b.it, b.status, b.author, b.trust, row_number() OVER (ORDER BY t.id) AS n
    FROM translations t JOIN best b ON b.hash = t.hash AND b.en = t.en
    WHERE t.trust < 2 AND t.trust < b.trust
)
UPDATE translations SET
    it = x.it, status = x.status, author = x.author, at = ?3, trust = x.trust,
    seq = max((SELECT coalesce(max(seq), 0) FROM translations), ?4) + x.n
FROM targets x
WHERE translations.id = x.id AND translations.hash = x.hash;
