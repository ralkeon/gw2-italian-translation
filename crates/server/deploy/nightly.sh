#!/bin/sh
# The nightly jobs, one after the other (cron on ifalna: 30 2 * * * sh $HOME/gw2it-sync/nightly.sh).
# Each may now run for hours (it waits for busy models): run together they would spend the same
# quota and the later ones in the order of priority.rs would work on rows the earlier ones are
# still changing. First the texts nobody has seen in the game yet, from the Local Translator
# module's map (mappa.sh, bulk.rs: at most 5000 a night, the rest the next nights), so that the
# jobs below take them too. Order: names (which names stay in English, the highest), then consistency (one
# form per name), then plurals (only the form, on top of any row), then the copy to the spare,
# which gets everything they changed.
cd "$HOME/gw2it-sync" || exit 1
exec 9> nightly.lock
flock -n 9 || { echo "$(date): nightly already running" >> nightly.log; exit 0; }
# A job started by hand (a long names run) finishes first. The pattern starts the command line: a
# shell that only names the job (a watcher waiting on it) is not the job, and it once held the
# night here for a whole day.
while pgrep -f "^[^ ]*gw2it-server (bulk|names|consistency|plurals)" > /dev/null; do sleep 60; done

sh ./mappa.sh
if [ -s mappa/map_it.db ]; then
    echo "=== $(date)" >> bulk.log
    ./gw2it-server bulk --db data/gw2it.db --file mappa/map_it.db --translators translators.json --max-texts 5000 >> bulk.log 2>&1
fi
echo "=== $(date)" >> names.log
./gw2it-server names --db data/gw2it.db --fetch --translators translators.json >> names.log 2>&1
echo "=== $(date)" >> consistency.log
./gw2it-server consistency --db data/gw2it.db --translators translators.json --max-groups 200 >> consistency.log 2>&1
echo "=== $(date)" >> plurals.log
./gw2it-server plurals --db data/gw2it.db --translators translators.json --max-texts 1500 >> plurals.log 2>&1
./gw2it-server replicate --db data/gw2it.db --to https://gw2it-sync.gw2it-sync.workers.dev --key-file replica.key >> replicate.log 2>&1
sh ./export.sh
