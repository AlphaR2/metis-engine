#!/usr/bin/env bash
# "Plays" a MOCK match to its end and waits for the markets on it to settle.
#   04-settle.sh <matchId> <homeGoals> <awayGoals>
# Sets the final score on both mock providers; the two keepers (separate reporter keys) report it, the
# oracle finalises on two agreeing reports, and after the dispute window (20s in the demo build) the
# crank settles. This is a demo path: the score is whatever you type here, and the UI should say so.
set -euo pipefail
. "$(dirname "$0")/common.sh"
MATCH=${1:?matchId} H=${2:?home goals} A=${3:?away goals}
grep -q "^$MATCH	" "$RUN/fixtures.tsv" || { echo "unknown match $MATCH (see $RUN/fixtures.tsv)"; exit 1; }
for i in 1 2; do python3 -I - "$RUN/mock-$i.json" "$MATCH" "$H" "$A" <<'PY'
import json,sys
p,mid,h,a=sys.argv[1:]
s=json.load(open(p)); s[mid]={"status":"FINISHED","duration":"REGULAR","home":int(h),"away":int(a),"ht_home":0,"ht_away":0}; json.dump(s,open(p,"w"))
PY
done
echo "mock result set: $MATCH $H-$A (MOCK data). waiting for settlement..."
ORA=$(awk -F'\t' -v m="$MATCH" '$1==m{print $3}' "$RUN/fixtures.tsv")
MARKETS=$(awk -F'\t' -v m="$MATCH" '$2==m{print $3}' "$RUN/markets.tsv")
for t in $(seq 1 360); do
  left=0
  for m in $MARKETS; do out=$($OPS show --market "$m" 2>&1 || true); case $out in *"status:        Settled"*) ;; *) left=$((left+1)) ;; esac; done
  [ $left -eq 0 ] && { echo "all markets on $MATCH settled"; for m in $MARKETS; do $OPS show --market "$m" | head -12; done; exit 0; }
  sleep 5
done
echo "not settled after ~30 min: check $RUN/keeper-1.log, keeper-2.log, crank.log"; exit 1
