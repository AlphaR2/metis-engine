#!/usr/bin/env bash
# Builds (does NOT sign or send) the Panta version of a demo question, through the running aggregator
# (devnet/run-aggregator.sh), so the same call exists on both venues for the Fill Score.
#   SITE_URL=https://your-app.vercel.app devnet/panta-create.sh <wallet> "<question>" "<resolution rule>" [hours_until_start, default 2]
# SITE_URL is where the cover image is fetched from: Panta must be able to reach it (no localhost), so it has to be your
# public site. The cover is the 1024x1024 PNG at <SITE_URL>/skins/sports.png (Panta recommends a 1024x1024 square).
# SOURCE_URL (optional) is where the result is read from, e.g. the competition's official fixtures page; it is sent as the
# source of truth and as the market's oracle. Default: https://www.fifa.com
# Panta derives a market's address from wallet + question, so the same wallet cannot create the same question twice
# (DUPLICATE_MARKET). This script only quotes and builds; nothing is created until you sign in the web app.
# The aggregator talks to Panta with its own PANTA_API_KEY; nothing here reads or prints it. Panta is
# MAINNET: the build step only quotes and reserves a ~5 minute session. Signing and broadcasting the
# returned transaction (in the web app) spends REAL mainnet SOL/USDC from <wallet>, including Panta's
# creation fee, which Panta prices in the quote (it is shown in the response `cost`).
set -euo pipefail
API=${API:-http://127.0.0.1:8080}
: "${SITE_URL:?set SITE_URL to your public site (Panta fetches the cover image from it), e.g. https://metis.vercel.app}"
WALLET=${1:?mainnet wallet that would pay and earn creator fees} Q=${2:?question} RULE=${3:?resolution rule} HRS=${4:-2}
NOW=$(date +%s); START=$((NOW + HRS*3600 + 600)); END=$((START + 2*3600)); RES=$((END + 3600))
curl -sS -X POST "$API/execution/markets" -H 'content-type: application/json' -d "$(python3 -I - "$WALLET" "$Q" "$RULE" "$START" "$END" "$RES" "${SITE_URL%/}" "${SOURCE_URL:-https://www.fifa.com}" <<'PY'
import json,sys
w,q,r,s,e,res,site,src=sys.argv[1:]
print(json.dumps({"venue_id":"panta","question":q,"resolution_rule":r,"sources_of_truth":[src],
 "category":"sports","start_time":int(s),"end_time":int(e),"resolution_time":int(res),
 "image_url":"%s/skins/sports.png" % site,"wallet":w,
 "description":"Football: %s Resolves from %s." % (q, src),
 "venue_params":{"oracle":src}}))
PY
)" | python3 -I -c "import json,sys; d=json.load(sys.stdin); t=d.get('transaction') or {}; print(json.dumps({k:(v if k!='serialized' and not (isinstance(v,str) and len(v)>120) else '<%d chars>'%len(v)) for k,v in d.items()}, indent=1)[:2500])"
