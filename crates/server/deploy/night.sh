#!/bin/sh
# The night on ifalna (owner 2026-11-19: production holds the agents' translations only). Cron:
#   30 2 * * * sh $HOME/gw2it-sync/night.sh
# One step after the other, each with a time limit (one that hangs is stopped and the night goes on); one line
# per step in night.log: start, end, rc (124 = limit).
#   gw2-update.sh            the official client (Wine) updates ~/gw2/client/Gw2.dat with -image, no login
#   ./gw2it-server seeds     the new seeds of Text Translator's shared pool -> ~/gw2/seeds.db (read only)
#   ./gw2it-server gw2dat    every English text of that Gw2.dat (plain + opened with the seeds) -> data/gw2dat_en.db,
#                            with French, German, Spanish (table other: references for the agents); the copy of the
#                            night before is kept as data/gw2dat_en.prev.db
#   agent/pilot/tools/feed.py  texts new or changed by a patch at or below the long run's last key ->
#                            agent/pilot/queue.txt, which the next batch takes first (the others come in key order)
#   agent/ltsync-pull.sh     the other module's repos (fetch + fast-forward only) and what changed in them
#   agent/ltreviewed.py      their hand-reviewed translations -> data/lt_reviewed.db (references for the agents)
#   nightly.sh publish       the export (data/gw2it.db -> export/gw2it.db.gz); no spare on Cloudflare since 2026-11-19
# The translating itself is the long run (agent/pilot/long.sh, its own cron), which writes into
# pilot/data/final.db; agent/pilot/tools/prod_merge.sh (cron, every 10 minutes) brings it into data/gw2it.db.
# No translator, no job rewrites production rows any more. The old night: night.sh.old-20261119.
cd "$HOME/gw2it-sync" || exit 1
exec 6> night.lock
flock -n 6 || { echo "$(date '+%F %T') night already running" >> night.log; exit 0; }
step() {  # name limit command...
    name=$1; limit=$2; shift 2
    echo "$(date '+%F %T') $name start" >> night.log
    timeout -k 120 "$limit" "$@" 6>&-
    echo "$(date '+%F %T') $name end rc=$?" >> night.log
}
step gw2-update 2h sh gw2-update.sh
step seeds 30m sh -c './gw2it-server seeds --out "$HOME/gw2/seeds.db" >> seeds.log 2>&1'
[ -s data/gw2dat_en.db ] && sqlite3 data/gw2dat_en.db ".backup data/gw2dat_en.prev.db"
step gw2dat 1h sh -c './gw2it-server gw2dat --dat "$HOME/gw2/client/Gw2.dat" --seeds "$HOME/gw2/seeds.db" --out data/gw2dat_en.db --db data/gw2it.db --file mappa/map_it.db >> gw2dat.log 2>&1'
step feed 10m sh -c 'python3 agent/pilot/tools/feed.py >> agent/pilot/feed.log 2>&1'
step pull 30m sh agent/ltsync-pull.sh
step lt-reviewed 10m sh -c 'python3 agent/ltreviewed.py >> ltreviewed.log 2>&1'
step publish 2h sh nightly.sh publish
echo "$(date '+%F %T') night done" >> night.log
