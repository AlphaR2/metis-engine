#!/usr/bin/env bash
# Pulls every market Panta's API will give us and dumps it to JSON.
#
#     bash scripts/dump-panta-catalog.sh [out.json]        # list pass only (fast)
#     bash scripts/dump-panta-catalog.sh [out.json] --live  # + live spot price per servable market
#
# Only `category` and `limit` are honoured as filters by Panta's /markets/ list endpoint
# (confirmed live — see scripts/check-panta.sh). `cursor` pagination was dead (same page back
# every time) until Panta fixed it venue-side on 2026-09-28 - re-verified live and it now
# advances correctly. This script still fans out across every category Panta's own
# `GET /categories/` reports, plus one unfiltered call as a cross-check, since that's needed
# regardless (the list is a registry, not a full chain scan - see services/panta/chain.rs).
# Results are deduped by marketId and split into servable vs not, with why.
#
# `--live` adds a second pass: Panta's own docs say list rows do NOT live-RPC for prices ("List
# rows do not live-RPC for prices (cost). Use Get market for spot prices when RPC is available.") —
# so for every market that survives filtering, this calls GET /markets/{id}/ and records the live
# price alongside the list-row price, so you can see whether they actually differ.
#
# "Servable" means: phase=primary, status not resolved/cancelled, endTime (or resolutionTime as
# fallback) still in the future, and some text (title or description) exists to show a user —
# the same bar aggregator/src/models/venue/mod.rs + models/venue/panta.rs apply.
#
# Reads PANTA_API_KEY / PANTA_BASE_URL from ./.env. Nothing is written except the output file;
# the key is never printed or included in the dump.
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1
[ -f .env ] || { echo "no .env here — run from the aggregator directory"; exit 1; }
set -a; . ./.env; set +a
: "${PANTA_API_KEY:?PANTA_API_KEY not set in .env}"
BASE="${PANTA_BASE_URL:-https://live-api.panta.market/api/v1}"

OUT="panta-catalog-dump.json"
LIVE=0
for arg in "$@"; do
  case "$arg" in
    --live) LIVE=1 ;;
    *) OUT="$arg" ;;
  esac
done

# Used only if GET /categories/ fails to answer — last known-good superset.
FALLBACK_CATEGORIES=(sports crypto politics entertainment finance science world other
                      business commodities gaming pop-culture stocks weather)

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
get() { curl -s -m 30 -H "X-Api-Key: $PANTA_API_KEY" "$BASE/$1"; }

echo "=============================================================="
echo " PANTA FULL CATALOG DUMP$([ "$LIVE" = 1 ] && echo ' (+ live prices)')"
echo "=============================================================="

# ── Categories: ask Panta rather than guess ─────────────────────────────────────────────────
echo
echo "Fetching category list from GET /categories/ ..."
get "categories/" > "$TMP/categories.json" 2>/dev/null
CATEGORIES=($(python3 -c "
import json,sys
try:
    d=json.load(open('$TMP/categories.json'))
except Exception:
    raise SystemExit
# Accept whatever shape comes back: a bare list, a list of objects, or a wrapper key.
if isinstance(d, list):
    items = d
elif isinstance(d, dict):
    items = d.get('categories') or d.get('items') or d.get('data') or []
else:
    items = []
out = []
for it in items:
    if isinstance(it, str):
        out.append(it)
    elif isinstance(it, dict):
        v = it.get('value') or it.get('slug') or it.get('name') or it.get('category') or it.get('id')
        if v:
            out.append(v)
print(chr(10).join(out))
" 2>/dev/null))

if [ "${#CATEGORIES[@]}" -eq 0 ]; then
  echo "    (GET /categories/ gave nothing usable — falling back to last known-good list)"
  CATEGORIES=("${FALLBACK_CATEGORIES[@]}")
else
  echo "    got ${#CATEGORIES[@]} categories live: ${CATEGORIES[*]}"
fi

echo
echo "Fetching ${#CATEGORIES[@]} categories + 1 unfiltered pass..."

: > "$TMP/all.jsonl"

fetch_one() {
  local label="$1" query="$2"
  get "markets/?$query" > "$TMP/resp.json" 2>/dev/null
  local n
  n=$(python3 -c "
import json,sys
try:
    d=json.load(open('$TMP/resp.json'))
except Exception:
    print(0); raise SystemExit
items=d.get('items',[])
with open('$TMP/all.jsonl','a') as f:
    for i in items:
        i['_seenVia']='$label'
        f.write(json.dumps(i)+chr(10))
print(len(items))
" 2>/dev/null)
  printf "    %-16s %s rows\n" "$label" "${n:-0 (request failed)}"
}

for c in "${CATEGORIES[@]}"; do
  fetch_one "category=$c" "limit=50&category=$c"
done
fetch_one "unfiltered" "limit=50"

echo
echo "Deduping and classifying..."

python3 - "$TMP/all.jsonl" "$TMP/classified.json" <<'PY'
import json, sys, time

src, out_path = sys.argv[1], sys.argv[2]
now = int(time.time())

by_id = {}
seen_via = {}
for line in open(src):
    line = line.strip()
    if not line:
        continue
    try:
        r = json.loads(line)
    except Exception:
        continue
    mid = r.get("marketId")
    if not mid:
        continue
    seen_via.setdefault(mid, set()).add(r.pop("_seenVia", "?"))
    by_id[mid] = r

def has_text(r):
    return bool((r.get("title") or "").strip()) or bool((r.get("description") or "").strip())

def end_time(r):
    return r.get("endTime") or r.get("resolutionTime") or 0

def why_dead(r):
    reasons = []
    if r.get("phase") not in (None, "primary", "secondary"):
        reasons.append(f"phase={r.get('phase')}")
    if r.get("status") in ("resolved", "cancelled"):
        reasons.append(f"status={r.get('status')}")
    if r.get("resolved"):
        reasons.append("resolved=true")
    et = end_time(r)
    if et and et <= now:
        reasons.append("betting window closed")
    if not has_text(r):
        reasons.append("no title or description")
    return reasons

servable, dead = [], []
for mid, r in by_id.items():
    r["_seenViaCategories"] = sorted(seen_via[mid])
    reasons = why_dead(r)
    if reasons:
        r["_excludedBecause"] = reasons
        dead.append(r)
    else:
        r["_closesInDays"] = round((end_time(r) - now) / 86400, 1)
        servable.append(r)

servable.sort(key=lambda r: end_time(r))

dump = {
    "fetchedAt": now,
    "totalDistinctMarkets": len(by_id),
    "servableCount": len(servable),
    "excludedCount": len(dead),
    "servable": servable,
    "excluded": dead,
}
json.dump(dump, open(out_path, "w"), indent=2)

print(f"    distinct markets total : {len(by_id)}")
print(f"    servable now           : {len(servable)}")
print(f"    excluded               : {len(dead)}")
if dead:
    from collections import Counter
    tally = Counter(reason for r in dead for reason in r["_excludedBecause"])
    print("    exclusion reasons:")
    for reason, n in tally.most_common():
        print(f"      {reason:<28} {n}")
PY

# ── Live pass: GET /markets/{id}/ for every servable market ────────────────────────────────
if [ "$LIVE" = 1 ]; then
  echo
  echo "Live pass: fetching GET /markets/{id}/ for every servable market..."
  IDS=($(python3 -c "
import json
d=json.load(open('$TMP/classified.json'))
print(chr(10).join(m['marketId'] for m in d['servable']))
"))
  echo "    ${#IDS[@]} markets to check"
  : > "$TMP/live.jsonl"
  i=0
  for id in "${IDS[@]}"; do
    i=$((i+1))
    get "markets/$id/" > "$TMP/one.json" 2>/dev/null
    python3 -c "
import json
try:
    d=json.load(open('$TMP/one.json'))
    d['marketId']=d.get('marketId') or '$id'
except Exception:
    d={'marketId':'$id','_liveFetchFailed':True}
print(json.dumps(d))
" >> "$TMP/live.jsonl" 2>/dev/null
    printf "\r    %d/%d" "$i" "${#IDS[@]}"
  done
  echo
fi

echo
echo "Merging and writing final dump..."

python3 - "$TMP/classified.json" "$([ "$LIVE" = 1 ] && echo "$TMP/live.jsonl" || echo "")" "$OUT" <<'PY'
import json, sys

classified_path, live_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
d = json.load(open(classified_path))

price_fields = ["yesPrice", "noPrice", "primaryYesPrice", "primaryNoPrice", "volumeUsdc"]

if live_path:
    live_by_id = {}
    for line in open(live_path):
        line = line.strip()
        if not line:
            continue
        try:
            r = json.loads(line)
        except Exception:
            continue
        live_by_id[r["marketId"]] = r

    changed = 0
    for m in d["servable"]:
        live = live_by_id.get(m["marketId"])
        if not live or live.get("_liveFetchFailed"):
            m["_live"] = {"fetchFailed": True}
            continue
        list_snapshot = {f: m.get(f) for f in price_fields}
        live_snapshot = {f: live.get(f) for f in price_fields}
        m["_live"] = {
            "listRowPrices": list_snapshot,
            "liveRpcPrices": live_snapshot,
            "differs": list_snapshot != live_snapshot,
        }
        if list_snapshot != live_snapshot:
            changed += 1

    d["liveComparisonCount"] = len(live_by_id)
    d["liveDifferedFromListCount"] = changed

json.dump(d, open(out_path, "w"), indent=2)

print(f"    distinct markets total : {d['totalDistinctMarkets']}")
print(f"    servable now           : {d['servableCount']}")
print(f"    excluded               : {d['excludedCount']}")
if live_path:
    print(f"    live-checked           : {d.get('liveComparisonCount', 0)}")
    print(f"    live price != list row : {d.get('liveDifferedFromListCount', 0)}")

if d["servable"]:
    print()
    print("    servable markets:")
    for r in d["servable"][:20]:
        text = (r.get("title") or r.get("description") or "(no text)")[:55]
        live_tag = ""
        if "_live" in r:
            live_tag = "  [live≠list]" if r["_live"].get("differs") else "  [live=list]" if "liveRpcPrices" in r["_live"] else "  [live fetch failed]"
        print(f"      [{r.get('category','?'):<12}] closes in {r['_closesInDays']:>5}d  {text}{live_tag}")
    if len(d["servable"]) > 20:
        print(f"      ... and {len(d['servable']) - 20} more")
PY

echo
echo "Full dump written to: $OUT"
echo "=============================================================="
