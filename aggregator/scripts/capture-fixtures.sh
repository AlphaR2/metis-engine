#!/usr/bin/env bash
# Captures raw venue payloads as test fixtures under aggregator/fixtures/<venue>/.
#
#     bash scripts/capture-fixtures.sh
#
# Read-only GETs against each venue's catalog endpoints - the same calls a refresh makes. Bodies
# are saved exactly as the venue sent them (no reshaping), one file per request, so parser, venue
# contract and matching tests run against real data. Each file's HTTP status is recorded in
# fixtures/<venue>/_index.tsv.
#
# Reads PANTA_API_KEY / JUPITER_API_KEY (and optional *_BASE_URL) from ./.env. Keys are sent as
# headers only and are never written to disk or printed.
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1
[ -f .env ] || { echo "no .env here - run from the aggregator directory"; exit 1; }
set -a; . ./.env; set +a

PANTA_BASE="${PANTA_BASE_URL:-https://live-api.panta.market/api/v1}"
JUP_BASE="${JUPITER_BASE_URL:-https://api.jup.ag/prediction/v1}"
PAUSE=0.4   # stay well under both venues' rate limits

# fetch <venue> <name> <url> <header>
fetch() {
  local venue="$1" name="$2" url="$3" header="$4"
  local dir="fixtures/$venue"
  mkdir -p "$dir"
  local status
  status=$(curl -sS -o "$dir/$name.json" -w '%{http_code}' -H "$header" --max-time 30 "$url") || status="curl-error"
  printf '%s\t%s\t%s\n' "$name" "$status" "${url#*://*/}" >> "$dir/_index.tsv"
  echo "$venue/$name -> $status"
  sleep "$PAUSE"
}

urlencode() { python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1]))' "$1"; }

# ── Panta ──────────────────────────────────────────────────────────────────────
if [ -n "${PANTA_API_KEY:-}" ]; then
  rm -rf fixtures/panta
  H="X-Api-Key: $PANTA_API_KEY"
  fetch panta categories "$PANTA_BASE/categories/" "$H"
  cats=$(python3 -c '
import json, sys
try:
    d = json.load(open("fixtures/panta/categories.json"))
except Exception:
    sys.exit(0)
rows = d if isinstance(d, list) else next((v for v in d.values() if isinstance(v, list)), []) if isinstance(d, dict) else []
for r in rows:
    v = r if isinstance(r, str) else (r.get("slug") or r.get("id") or r.get("name") or r.get("value")) if isinstance(r, dict) else None
    if v: print(v)
')
  [ -n "$cats" ] || cats=$'sports\ncrypto\npolitics\nentertainment\nfinance\nscience\nworld\nother'
  fetch panta markets-all "$PANTA_BASE/markets/?limit=50" "$H"
  while IFS= read -r c; do
    [ -n "$c" ] && fetch panta "markets-$(echo "$c" | tr ' &/' '___')" "$PANTA_BASE/markets/?category=$(urlencode "$c")&limit=50" "$H"
  done <<< "$cats"
  first=$(python3 -c '
import json
d = json.load(open("fixtures/panta/markets-all.json"))
items = d.get("items", []) if isinstance(d, dict) else d
print(items[0]["marketId"] if items else "")
' 2>/dev/null)
  [ -n "$first" ] && fetch panta "market-$first" "$PANTA_BASE/markets/$first/" "$H"
  # List rows carry no text (title/description are blank); the question, rules and sources are
  # only on the single-market endpoint. Capture it for every market seen, one indexed file each,
  # so the venue contract tests can serve GET /markets/{id}/ like Panta does.
  ids=$(python3 -c '
import json, glob
seen = set()
for f in sorted(glob.glob("fixtures/panta/markets-*.json")):
    try:
        for r in json.load(open(f)).get("items", []):
            if r["marketId"] not in seen:
                seen.add(r["marketId"]); print(r["marketId"])
    except Exception:
        pass
')
  while IFS= read -r id; do
    [ -n "$id" ] && [ ! -f "fixtures/panta/market-$id.json" ] && fetch panta "market-$id" "$PANTA_BASE/markets/$id/" "$H"
  done <<< "$ids"
else
  echo "PANTA_API_KEY not set - skipping Panta"
fi

# ── Jupiter ────────────────────────────────────────────────────────────────────
if [ -n "${JUPITER_API_KEY:-}" ]; then
  rm -rf fixtures/jupiter
  H="x-api-key: $JUPITER_API_KEY"
  # Probe for a categories endpoint; a 404 here is itself an answer.
  fetch jupiter categories-probe "$JUP_BASE/categories" "$H"
  fetch jupiter events-all "$JUP_BASE/events?start=0&end=50&includeMarkets=true&sortBy=volume&sortDirection=desc" "$H"
  for c in crypto sports politics esports culture economics tech finance "climate & science" weather mentions; do
    fetch jupiter "events-$(echo "$c" | tr ' &/' '___')" "$JUP_BASE/events?start=0&end=50&includeMarkets=true&sortBy=volume&sortDirection=desc&category=$(urlencode "$c")" "$H"
  done
  first=$(python3 -c '
import json
d = json.load(open("fixtures/jupiter/events-sports.json"))
for e in d.get("data", []):
    for m in e.get("markets", []):
        if m.get("status") == "open":
            print(m["marketId"]); raise SystemExit
' 2>/dev/null)
  if [ -n "$first" ]; then
    fetch jupiter "market-$first" "$JUP_BASE/markets/$first" "$H"
    fetch jupiter "orderbook-$first" "$JUP_BASE/orderbook/$first" "$H"
  fi
else
  echo "JUPITER_API_KEY not set - skipping Jupiter"
fi

date -u +%s > fixtures/captured_at
echo "done - see fixtures/*/_index.tsv"
