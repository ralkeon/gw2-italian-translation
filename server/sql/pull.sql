-- What changed after ?1, oldest first, at most ?2 rows.
SELECT id, hash, en, it, status, author, seq FROM translations
WHERE seq > ?1 ORDER BY seq LIMIT ?2;
