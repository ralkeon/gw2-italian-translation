#!/bin/sh
# Copies the global database from ifalna to server/ifalna.db, to look at it (PhpStorm data source
# "ifalna"). A consistent snapshot (sqlite3 .backup), safe while the server runs. Changes made to
# the copy stay here.
set -e
here=$(dirname "$0")
ssh ifalna 'sqlite3 ~/gw2it-sync/data/gw2it.db ".backup /tmp/gw2it-snapshot.db"'
scp -q ifalna:/tmp/gw2it-snapshot.db "$here/ifalna.db"
ssh ifalna 'rm -f /tmp/gw2it-snapshot.db'
echo "$here/ifalna.db: $(wc -c < "$here/ifalna.db") bytes"
