#!/usr/bin/env bash
# Creates demo fixtures and Metis markets (creator-seeded, 30 USDC seed each, demo USDC).
#
#   03-markets.sh seed                         the standing board: 4 markets on 2 fixtures that stay open
#   03-markets.sh live [lead_secs]             one fast market (default 330s to kickoff) for the live settle demo
#   03-markets.sh backed [seed_usdc] [raise_secs]   one DBC-backed market: a crowd raises the seed on a Meteora curve
#                                              (needs devnet/dbc.sh run first). Betting opens when the raise activates it.
#   03-markets.sh one <matchId> <home> <away> <lead_secs> <condition> <question> <rubric_yes> <label_yes> <label_no>
#
# Betting stays open until (kickoff - 120s) in the demo build. Fixtures are MOCK matches: the result comes
# from the labelled mock provider (devnet/04-settle.sh), never from real football data.
# The same question is also what you create on Panta (devnet/panta-create.sh) for the Fill Score pair.
set -euo pipefail
. "$(dirname "$0")/common.sh"
[ -f "$STATE" ] || { echo "run devnet/01-setup.sh first"; exit 1; }
LOG=$RUN/markets.tsv; touch "$LOG" "$RUN/fixtures.tsv"
NONCE_FILE=$D/nonce; [ -f "$NONCE_FILE" ] || echo 0 > "$NONCE_FILE"
NO_RESTART=${NO_RESTART:-}

one() {  # matchId home away lead condition question rubric_yes label_yes label_no
  local MATCH=$1 HOME_T=$2 AWAY_T=$3 LEAD=$4 COND=$5 Q=$6 RY=$7 LY=$8 LN=$9
  local NONCE=$(( $(cat "$NONCE_FILE") + 1 )); echo $NONCE > "$NONCE_FILE"
  local KICK=$(( $(date +%s) + LEAD ))
  local OUT ROOT ORA MARKET MARKET_OUT
  # one oracle per fixture: reuse it if this match already has one
  if grep -q "^$MATCH	" "$RUN/fixtures.tsv"; then
    ROOT=$(awk -F'\t' -v m="$MATCH" '$1==m{print $2}' "$RUN/fixtures.tsv"); ORA=$(awk -F'\t' -v m="$MATCH" '$1==m{print $3}' "$RUN/fixtures.tsv")
    KICK=$(awk -F'\t' -v m="$MATCH" '$1==m{print $4}' "$RUN/fixtures.tsv")   # the fixture's own kickoff
  else
    OUT=$(KEEPER_KEYPAIR=$D/reporter1.json $KEEPER create-fixture --kickoff $KICK --id football-data=$MATCH)
    ROOT=$(echo "$OUT" | awk '/^fixture_root:/{print $2}'); ORA=$(echo "$OUT" | awk '/^oracle:/{print $2}')
    [ -n "$ROOT" ] && [ -n "$ORA" ] || { echo "create-fixture failed: $OUT"; exit 1; }
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$MATCH" "$ROOT" "$ORA" "$KICK" "$HOME_T" "$AWAY_T" >> "$RUN/fixtures.tsv"
    python3 -I - "$RUN/watch.json" "$ROOT" "$MATCH" <<'PY'
import json,sys
p,root,mid=sys.argv[1:]
w=json.load(open(p)); w.append({"fixture_root":root,"external_id":mid}); json.dump(w,open(p,"w"))
PY
    for i in 1 2; do python3 -I - "$RUN/mock-$i.json" "$MATCH" <<'PY'
import json,sys
p,mid=sys.argv[1:]
s=json.load(open(p)); s[mid]={"status":"SCHEDULED","home":None,"away":None}; json.dump(s,open(p,"w"))
PY
    done
  fi
  local COMMON=(--creator "$D/creator.json" --mint "$MINT" --oracle "$ORA" --nonce $NONCE --condition "$COND"
    --question "$Q" --rubric-yes "$RY"
    --rubric-no "NO otherwise, after 90 minutes. Extra time and penalties do not count."
    --label-yes "$LY" --label-no "$LN" --league "Demo (mock match)")
  if [ -n "${BACKED_SEED:-}" ]; then
    local RES BMINT; RES=$("$ENGINE/devnet/dbc.sh" reserve "$NONCE" "$BACKED_SEED") || { echo "reserve failed: $RES"; exit 1; }
    BMINT=$(echo "$RES" | python3 -I -c "import json,sys; print(json.loads(sys.stdin.read().strip().splitlines()[-1])['backerMint'])")
    MARKET_OUT=$($OPS create-backed-market "${COMMON[@]}" --backer-mint "$BMINT" --seed "$BACKED_SEED" --raise-secs "${RAISE_SECS:-300}")
  else
    MARKET_OUT=$($OPS create-market "${COMMON[@]}")
  fi
  MARKET=$(echo "$MARKET_OUT" | awk '/^market:/{print $2}')
  [ -n "$MARKET" ] || { echo "create-market failed: $MARKET_OUT"; exit 1; }
  printf '%s\t%s\t%s\t%s\t%s\n' "$NONCE" "$MATCH" "$MARKET" "$COND" "$Q" >> "$LOG"
  echo "market $MARKET  (nonce $NONCE, match $MATCH, kickoff $KICK, betting closes $((KICK - 120)))  $Q"
}

case ${1:?seed|live|backed|one} in
  seed)
    # Match ids are unique per run: the oracle account of a match is derived from its id, so a fixed id could
    # only ever be registered once (a second run would fail with oracle error 6002 "already exists").
    M1=$(( 100000 + $(date +%s) - 1790000000 )); M2=$(( M1 + 1 ))
    one $M1 Nigeria Ghana 21600 home-win "Will Nigeria beat Ghana?" "YES if Nigeria lead after 90 minutes, stoppage time included." "Nigeria win" "Draw or Ghana"
    one $M1 Nigeria Ghana 21600 home-win-by-over:1 "Will Nigeria beat Ghana by 2 or more?" "YES if Nigeria win by two goals or more after 90 minutes." "Nigeria by 2+" "Anything else"
    one $M2 Senegal Cameroon 108000 home-win "Will Senegal beat Cameroon?" "YES if Senegal lead after 90 minutes, stoppage time included." "Senegal win" "Draw or Cameroon"
    one $M2 Senegal Cameroon 108000 goals-over:2 "Will Senegal v Cameroon have 3 or more goals?" "YES if three or more goals are scored in 90 minutes." "3+ goals" "2 or fewer" ;;
  live)
    ID=$(( 91000 + $(cat "$NONCE_FILE") + 1 ))
    one $ID Nigeria Ghana "${2:-330}" home-win "Will Nigeria beat Ghana? (live demo $ID)" "YES if Nigeria lead after 90 minutes, stoppage time included." "Nigeria win" "Draw or Ghana"
    echo "settle it with: devnet/04-settle.sh $ID 2 0" ;;
  one) shift; one "$@" ;;
  backed)
    BACKED_SEED=${2:-30} RAISE_SECS=${3:-300}
    ID=$(( 92000 + $(cat "$NONCE_FILE") + 1 ))
    one $ID Nigeria Ghana $(( RAISE_SECS + 420 )) home-win "Will Nigeria beat Ghana? (crowd-seeded $ID)" "YES if Nigeria lead after 90 minutes, stoppage time included." "Nigeria win" "Draw or Ghana"
    echo "back the raise with: devnet/dbc.sh back <market> <usdc>   |   settle it with: devnet/04-settle.sh $ID 2 0" ;;
  *) echo "usage: 03-markets.sh seed|live|backed|one"; exit 2 ;;
esac
# keepers read the watch file at start: restart them so they pick the new fixtures up
if [ -z "$NO_RESTART" ] && [ -f "$RUN/keeper-1.pid" ]; then "$ENGINE/devnet/02-start.sh" >/dev/null && echo "keepers restarted"; fi
