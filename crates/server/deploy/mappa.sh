#!/bin/sh
# The Local Translator module's map (its private dev repository, release "mappa", map_it.db.zip),
# downloaded for the IDs and the English of the texts we do not have yet (bulk.rs reads it). Needs
# gh-mappa.token: a GitHub token that can read that repository (chmod 600). Kept in mappa/, which
# Apache does not serve. On a failure the last map stays and bulk works on it.
cd "$HOME/gw2it-sync" || exit 1
[ -s gh-mappa.token ] || { echo "$(date): mappa: no gh-mappa.token" >> mappa.log; exit 0; }
mkdir -p mappa
API=https://api.github.com/repos/NikeGipple/gw2-nexus-local-translator-dev
# The token goes in on stdin, never on the command line.
auth() { printf 'Authorization: Bearer %s\n' "$(cat gh-mappa.token)"; }
asset=$(auth | curl -sf -H @- "$API/releases/tags/mappa" |
    python3 -c 'import json, sys; print(next(a["id"] for a in json.load(sys.stdin)["assets"] if a["name"] == "map_it.db.zip"))') ||
    { echo "$(date): mappa: release not readable (token expired?)" >> mappa.log; exit 0; }
auth | curl -sfL -H @- -H "Accept: application/octet-stream" -o mappa/map_it.db.zip.tmp "$API/releases/assets/$asset" ||
    { echo "$(date): mappa: download failed" >> mappa.log; exit 0; }
python3 -c 'import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); open(sys.argv[2], "wb").write(z.read("map_it.db"))' \
    mappa/map_it.db.zip.tmp mappa/map_it.db.tmp &&
    mv -f mappa/map_it.db.tmp mappa/map_it.db && rm -f mappa/map_it.db.zip.tmp ||
    { echo "$(date): mappa: bad zip" >> mappa.log; exit 0; }
chmod 600 mappa/map_it.db
echo "$(date): mappa $(stat -c %s mappa/map_it.db) bytes" >> mappa.log
