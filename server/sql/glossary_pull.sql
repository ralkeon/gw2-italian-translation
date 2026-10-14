-- What changed in the glossary after ?1, oldest first, at most ?2 rows.
SELECT en, it, wrong, author, seq FROM glossary WHERE seq > ?1 ORDER BY seq LIMIT ?2;
