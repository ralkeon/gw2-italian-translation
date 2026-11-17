#!/bin/sh
# The nightly jobs, in three parts that night.sh runs at different points of the night (cron on ifalna:
# 30 2 * * * sh $HOME/gw2it-sync/night.sh, which has the whole order):
#   sh nightly.sh jobs      map + bulk, names, plurals: first thing, before the agents audit them
#   sh nightly.sh lt-sync   the copy for the Local Translator's glossary, after the agents fixed our rows
#   sh nightly.sh publish   the copy to the spare and the export, last: they take everything the night changed
#   sh nightly.sh           all three, one after the other (by hand)
# Each may now run for hours (it waits for busy models): run together they would spend the same
# quota and the later ones in the order of priority.rs would work on rows the earlier ones are
# still changing. First the texts nobody has seen in the game yet, from the Local Translator
# module's map (mappa.sh, bulk.rs: at most 25000 a night since 2026-11-17, the rest the next nights), so that the
# jobs below take them too. Order: names (which names stay in English, the highest), then plurals
# (only the form, on top of any row).
# Not consistency (one form per name) since 2026-11-08: with the names in English it found nothing
# to rewrite for ten nights and only spent the quota of the review; run it by hand if ever needed.
cd "$HOME/gw2it-sync" || exit 1
part=${1:-all}
case "$part" in all|jobs|lt-sync|publish) ;; *) echo "usage: sh nightly.sh [jobs|lt-sync|publish]"; exit 1 ;; esac
exec 9> nightly.lock
flock -n 9 || { echo "$(date): nightly already running" >> nightly.log; exit 0; }

if [ "$part" = all ] || [ "$part" = jobs ]; then
    # A job started by hand (a long names run) finishes first. The pattern starts the command line: a
    # shell that only names the job (a watcher waiting on it) is not the job, and it once held the
    # night here for a whole day.
    while pgrep -f "^[^ ]*gw2it-server (bulk|names|consistency|plurals)" > /dev/null; do sleep 60; done
    sh ./mappa.sh
    if [ -s mappa/map_it.db ]; then
        echo "=== $(date)" >> bulk.log
        ./gw2it-server bulk --db data/gw2it.db --file mappa/map_it.db --translators translators.json --max-texts 25000 >> bulk.log 2>&1
    fi
    # Then the texts read from Gw2.dat that nobody has seen in the game yet (gw2dat.rs, run by night.sh
    # before this): after the map's, which players do meet; at most 10000 a night.
    if [ -s data/gw2dat_en.db ]; then
        echo "=== $(date) gw2dat" >> bulk.log
        ./gw2it-server bulk --db data/gw2it.db --file data/gw2dat_en.db --translators translators.json --max-texts 10000 >> bulk.log 2>&1
    fi
    echo "=== $(date)" >> names.log
    ./gw2it-server names --db data/gw2it.db --fetch --translators translators.json >> names.log 2>&1
    echo "=== $(date)" >> plurals.log
    ./gw2it-server plurals --db data/gw2it.db --translators translators.json --max-texts 1500 >> plurals.log 2>&1
fi

if [ "$part" = all ] || [ "$part" = lt-sync ]; then
    # The copy of our translations adapted to the Local Translator's glossary (lt_sync.rs): only its own
    # table, lt_sync; after the jobs (they come first for the quota) and after the agents that fix our
    # rows, so that it adapts them as they are now and not what the morning will undo; before the export,
    # which takes it into the download. The glossary's address in lt-sync.url (not in the repository);
    # without it, nothing.
    if [ -s lt-sync.url ]; then
        echo "=== $(date)" >> lt-sync.log
        ./gw2it-server lt-sync --db data/gw2it.db --glossary "$(cat lt-sync.url)" --translators translators.json --max-texts 2000 >> lt-sync.log 2>&1
    fi
fi

if [ "$part" = all ] || [ "$part" = publish ]; then
    ./gw2it-server replicate --db data/gw2it.db --to https://gw2it-sync.gw2it-sync.workers.dev --key-file replica.key >> replicate.log 2>&1
    sh ./export.sh
fi
