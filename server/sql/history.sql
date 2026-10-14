-- What a stronger row is about to replace.
-- ?1 JSON array of [id, hash, en, it, status, author, trust] rows (author and trust may be null: the
-- sender, and 2 for a hand correction, 0 for a machine translation; they are set when a server
-- merges another server's rows: see replicate in crates/server), ?2 the sender, ?3 the time (unix ms).
-- Same condition as upsert.sql.
INSERT INTO history (id, hash, it, status, author, at, replaced_by, replaced_at)
SELECT t.id, t.hash, t.it, t.status, t.author, t.at, coalesce(r.author, ?2), ?3
FROM (SELECT json_extract(j.value, '$[0]') AS id, json_extract(j.value, '$[1]') AS hash,
    json_extract(j.value, '$[2]') AS en, json_extract(j.value, '$[3]') AS it,
    json_extract(j.value, '$[4]') AS status, json_extract(j.value, '$[5]') AS author,
    coalesce(json_extract(j.value, '$[6]'), CASE json_extract(j.value, '$[4]') WHEN 'manual' THEN 2 ELSE 0 END) AS trust
    FROM json_each(?1) j) r
JOIN translations t ON t.id = r.id AND t.hash = r.hash
WHERE r.trust > t.trust OR (r.trust >= 2 AND r.trust = t.trust AND r.it <> t.it);
