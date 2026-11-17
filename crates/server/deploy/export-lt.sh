#!/bin/sh
# The copy of our translations adapted to the Local Translator's glossary (lt_sync.rs), to download:
# export-lt/lt_sync.db.gz, served by Apache behind its own password (apache-gw2it-lt.conf), made at
# the end of nightly.sh. The table lt_sync (id, hash, en, it, source, at) alone, without what the job
# keeps for itself (from_it, rule, tries).
cd "$HOME/gw2it-sync" || exit 1
[ -s data/lt_sync.db ] || exit 0
mkdir -p export-lt
rm -f export-lt/lt_sync.tmp.db
sqlite3 data/lt_sync.db ".backup export-lt/lt_sync.tmp.db" || exit 1
sqlite3 export-lt/lt_sync.tmp.db "ALTER TABLE lt_sync DROP COLUMN from_it; ALTER TABLE lt_sync DROP COLUMN rule;
    ALTER TABLE lt_sync DROP COLUMN tries; PRAGMA journal_mode = DELETE; VACUUM;" > /dev/null || exit 1
gzip -9 -c export-lt/lt_sync.tmp.db > export-lt/lt_sync.db.gz.tmp && mv -f export-lt/lt_sync.db.gz.tmp export-lt/lt_sync.db.gz
rm -f export-lt/lt_sync.tmp.db
chmod 644 export-lt/lt_sync.db.gz
echo "$(date): export $(stat -c %s export-lt/lt_sync.db.gz) bytes" >> export-lt.log
