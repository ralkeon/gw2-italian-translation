#!/bin/sh
# A copy of the database to download (export/gw2it.db.gz, served by Apache behind a password, see
# apache-gw2it.conf), made at the end of nightly.sh. Without what is not the translations': the
# signing secret (meta), the users' token hashes, the usage by IP and the replica's state.
cd "$HOME/gw2it-sync" || exit 1
mkdir -p export
rm -f export/gw2it.tmp.db
sqlite3 data/gw2it.db ".backup export/gw2it.tmp.db" || exit 1
sqlite3 export/gw2it.tmp.db "DROP TABLE IF EXISTS users; DROP TABLE IF EXISTS usage; DROP TABLE IF EXISTS usage_ip;
    DROP TABLE IF EXISTS review_usage; DROP TABLE IF EXISTS replica_state; DELETE FROM meta; VACUUM;" || exit 1
gzip -9 -c export/gw2it.tmp.db > export/gw2it.db.gz.tmp && mv -f export/gw2it.db.gz.tmp export/gw2it.db.gz
rm -f export/gw2it.tmp.db
chmod 644 export/gw2it.db.gz
echo "$(date): export $(stat -c %s export/gw2it.db.gz) bytes" >> export.log
