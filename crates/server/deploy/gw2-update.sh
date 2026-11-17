#!/bin/sh
# The official Guild Wars 2 client on ifalna (Wine), only to keep ~/gw2/client/Gw2.dat updated like a player's:
# Gw2-64.exe -image downloads the patch (if any) and exits, no login. Run by night.sh first; by hand: sh gw2-update.sh
# The launcher starts a copy of itself (Gw2-64.tmp): wineserver -w waits for it before the virtual screen closes.
# Read-only for everything else: the string extractor reads the .dat after this step.
cd "$HOME/gw2/client" || exit 1
export WINEPREFIX="$HOME/gw2/wineprefix" WINEDEBUG=-all
before="$(stat -c '%s %Y' Gw2.dat)"
xvfb-run -a -s "-screen 0 1280x800x24" sh -c "wine Gw2-64.exe -image; wineserver -w" > "$HOME/gw2/image-last.log" 2>&1
rc=$?
after="$(stat -c '%s %Y' Gw2.dat)"
echo "$(date '+%F %T') gw2-update rc=$rc dat $before -> $after$([ "$before" != "$after" ] && echo ' (PATCHED)')" >> "$HOME/gw2it-sync/gw2-update.log"
exit $rc
