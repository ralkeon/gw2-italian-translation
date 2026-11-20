#!/bin/sh
# The parts of the night that night.sh runs (cron on ifalna: 30 2 * * * sh $HOME/gw2it-sync/night.sh, which
# has the whole order):
#   sh nightly.sh lt-sync   the copy for the Local Translator's glossary (not in night.sh now: by hand)
#   sh nightly.sh publish   the export, last: it takes everything the night changed (no spare any more:
#                           the Cloudflare copy was removed 2026-11-19)
#   sh nightly.sh           both, one after the other (by hand)
# The jobs of the machine translations (bulk with the translators, names, plurals) were removed on 2026-11-20:
# the agents translate now (agent/pilot/long.sh).
cd "$HOME/gw2it-sync" || exit 1
part=${1:-all}
case "$part" in all|lt-sync|publish) ;; *) echo "usage: sh nightly.sh [lt-sync|publish]"; exit 1 ;; esac
exec 9> nightly.lock
flock -n 9 || { echo "$(date): nightly already running" >> nightly.log; exit 0; }

if [ "$part" = all ] || [ "$part" = lt-sync ]; then
    # The copy of our translations adapted to the Local Translator's glossary (lt_sync.rs): only its own
    # table, lt_sync; before the export, which takes it into the download. Without a model: only what
    # needs none (the rest by hand, ltsync-hand/round.sh). The glossary's address in lt-sync.url (not in
    # the repository); without it, nothing.
    if [ -s lt-sync.url ]; then
        echo "=== $(date)" >> lt-sync.log
        ./gw2it-server lt-sync --db data/gw2it.db --glossary "$(cat lt-sync.url)" --max-texts 2000 >> lt-sync.log 2>&1
    fi
fi

if [ "$part" = all ] || [ "$part" = publish ]; then
    sh ./export.sh
fi
