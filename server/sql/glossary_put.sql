-- A term of the glossary, already merged with the one there (gw2it_text::glossary::merge): ?1 the
-- English word, ?2 the Italian, ?3 the wrong forms (JSON array), ?4 who, ?5 when (unix ms), ?6 the
-- trust, ?7 the lowest seq this server hands out (0 on the main server, 10^12 on the spare).
INSERT OR REPLACE INTO glossary (en, it, wrong, author, at, seq, trust)
VALUES (?1, ?2, ?3, ?4, ?5, max((SELECT coalesce(max(seq), 0) FROM glossary), ?7) + 1, ?6);
