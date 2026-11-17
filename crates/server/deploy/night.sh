#!/bin/sh
# The whole night on ifalna, one step after the other (cron: 30 2 * * * sh $HOME/gw2it-sync/night.sh).
# No fixed times: each step starts when the one before has finished, and runs at most its time limit (one
# that hangs is stopped and the night goes on). One line per step in night.log: start, end, rc (124 = limit).
#   0. gw2-update.sh            the official client (Wine) updates ~/gw2/client/Gw2.dat with -image, no login
#      ./gw2it-server seeds    the new seeds of Text Translator's shared pool -> ~/gw2/seeds.db (read only)
#      ./gw2it-server gw2dat   every English text of that Gw2.dat (plain + opened with ~/gw2/seeds.db) -> data/gw2dat_en.db,
#                               with the same texts in French, German, Spanish (table other: references for the agents)
#      agent/ltreviewed.py      the Local Translator's hand-reviewed translations -> data/lt_reviewed.db (agents' refs)
#                               + agent/hybrid/lt-reviewed.tsv, those that differ from ours, for the hybrid agent
#   1. nightly.sh jobs          map + bulk, names, plurals; then byhand/case.py: case follows the English (Title Case
#                               names and labels), also on what the addons sent during the day
#   2. agent/agent.sh           the supervisor: audits what the jobs changed, fixes rows and code (may deploy)
#   3. agent/agent-hybrid.sh    our hybrid translation by families (after the supervisor: shared daily row cap)
#      (the full-Italian agent goes here)
#      byhand/case.py again, on what the agents wrote
#      ./gw2it-server full     the all-Italian variant: names at once, stale rows dropped (full.log)
#   4. nightly.sh lt-sync       the copy for the Local Translator's glossary, from our rows as the agents left them
#   5. agent/agent-ltsync.sh    the lt_sync agent (pulls the other author's repos itself), on what step 4 left pending
#      agent/agent-translate.sh  the translator agent: texts we lack, translated by Claude by rounds
#      agent/agent-full.sh       the full-Italian agent: the all-Italian rows rewritten by Claude by rounds
#   6. nightly.sh publish       copy to the spare and export, last: they take everything the night changed
# Every script still runs alone by hand; its own lock keeps two copies of it from running together.
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
step gw2dat 1h sh -c './gw2it-server gw2dat --dat "$HOME/gw2/client/Gw2.dat" --seeds "$HOME/gw2/seeds.db" --out data/gw2dat_en.db --db data/gw2it.db --file mappa/map_it.db >> gw2dat.log 2>&1'
step lt-reviewed 10m sh -c 'python3 agent/ltreviewed.py >> ltreviewed.log 2>&1'
step jobs 10h sh nightly.sh jobs
step case 30m sh -c 'venv/bin/python byhand/case.py --apply >> case.log 2>&1'
step supervisor 4h sh agent/agent.sh
step hybrid 4h sh agent/agent-hybrid.sh
step case 30m sh -c 'venv/bin/python byhand/case.py --apply >> case.log 2>&1'
step full 30m sh -c './gw2it-server full --db data/gw2it.db --names repo/full/names_it.toml --terms repo/full/terms.toml >> full.log 2>&1'
step lt-sync 4h sh nightly.sh lt-sync
step ltsync-agent 4h sh agent/agent-ltsync.sh
step translate 4h sh agent/agent-translate.sh
step full-agent 4h sh agent/agent-full.sh
step publish 2h sh nightly.sh publish
echo "$(date '+%F %T') night done" >> night.log
