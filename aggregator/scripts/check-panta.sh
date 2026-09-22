#!/usr/bin/env bash
# Re-checks the four structural defects found in Panta's /markets API on 2026-09-18.
#
# Run from the aggregator directory:
#     bash scripts/check-panta.sh
#
# Reads PANTA_API_KEY and PANTA_BASE_URL from ./.env. Nothing is written anywhere and the key is
# never printed.
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1
[ -f .env ] || { echo "no .env here — run from the aggregator directory"; exit 1; }
set -a; . ./.env; set +a
: "${PANTA_API_KEY:?PANTA_API_KEY not set in .env}"
BASE="${PANTA_BASE_URL:-https://live-api.panta.market/api/v1}"

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
get() { curl -s -m 30 -H "X-Api-Key: $PANTA_API_KEY" "$BASE/markets/?$1"; }

echo "=============================================================="
echo " PANTA STRUCTURAL RE-CHECK"
echo "=============================================================="

# ── 1. Cursor pagination ────────────────────────────────────────────────
# Was broken: nextCursor came back identical every page, so six requests returned
# 300 rows containing only 50 distinct markets.
echo
echo "[1] CURSOR PAGINATION — does following nextCursor advance?"
echo "    Tested twice: with a category filter and without, because the two may differ."
echo "    (Our refresh fans out BY category, so the filtered path is the one that matters to us.)"

walk() {
  local label="$1" base="$2"
  local cursor="" pages=0 prev=""
  : > "$TMP/ids"
  for _ in 1 2 3 4; do
    local q="$base"
    [ -n "$cursor" ] && q="$q&cursor=$cursor"
    get "$q" > "$TMP/page.json" 2>/dev/null || break
    python3 - "$TMP/page.json" "$TMP/ids" <<'PY2' 2>/dev/null || break
import json,sys
d=json.load(open(sys.argv[1]))
with open(sys.argv[2],"a") as f:
    for i in d.get("items",[]): f.write(i["marketId"]+"\n")
PY2
    local next
    next=$(python3 -c "import json;print(json.load(open('$TMP/page.json')).get('nextCursor') or '')" 2>/dev/null)
    pages=$((pages+1))
    [ -z "$next" ] && break
    [ "$next" = "$prev" ] && break
    prev="$next"; cursor="$next"
  done
  local total uniq
  total=$(wc -l < "$TMP/ids"); uniq=$(sort -u "$TMP/ids" | wc -l)
  printf "    %-22s pages=%s rows=%s distinct=%s  " "$label" "$pages" "$total" "$uniq"
  if [ "$total" -gt 0 ] && [ "$uniq" -lt "$total" ]; then
    echo "STILL BROKEN (repeats)"
  elif [ "$pages" -le 1 ]; then
    echo "only one page available"
  else
    echo "WORKS"
  fi
}

walk "no category filter" "limit=50"
walk "category=sports"    "limit=50&category=sports"

# ── 2. Filter params ────────────────────────────────────────────────────
# Was broken: status/phase/resolved were ignored; the first page came back
# byte-identical whatever was attached.
echo
echo "[2] FILTER PARAMS — is ?status=primary honoured?"
get "limit=30&status=primary&category=sports" > "$TMP/f.json"
python3 - "$TMP/f.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1])); it=d.get("items",[])
phases=sorted({i.get("phase","?") for i in it})
print(f"    n={len(it)}  phases={phases}")
print("    VERDICT: FIXED — only primary returned." if phases in ([],["primary"])
      else "    VERDICT: STILL IGNORED — non-primary rows came back.")
PY

# ── 3. Determinism ──────────────────────────────────────────────────────
# Was broken: two identical calls returned different phase mixes.
echo
echo "[3] DETERMINISM — do two identical calls agree?"
get "limit=20&category=sports" > "$TMP/a.json"; sleep 1
get "limit=20&category=sports" > "$TMP/b.json"
python3 - "$TMP/a.json" "$TMP/b.json" <<'PY'
import json,sys
a=[i["marketId"] for i in json.load(open(sys.argv[1])).get("items",[])]
b=[i["marketId"] for i in json.load(open(sys.argv[2])).get("items",[])]
print(f"    call A: {len(a)} rows   call B: {len(b)} rows")
print("    VERDICT: FIXED — identical results." if a==b
      else f"    VERDICT: STILL NONDETERMINISTIC — {len(set(a)^set(b))} ids differ.")
PY

# ── 4. Titles and catalog size ──────────────────────────────────────────
# Was broken: 0 of 300 rows carried a title; description was often empty too.
echo
echo "[4] TITLES + CATALOG SIZE — across every category"
: > "$TMP/all.jsonl"
for c in sports crypto politics entertainment finance science world other; do
  get "limit=50&category=$c" | python3 -c "
import json,sys
try: d=json.load(sys.stdin)
except: raise SystemExit
for i in d.get('items',[]): print(json.dumps(i))
" >> "$TMP/all.jsonl"
done
python3 - "$TMP/all.jsonl" <<'PY'
import json,sys,time
rows={}
for line in open(sys.argv[1]):
    try: r=json.loads(line); rows[r["marketId"]]=r
    except: pass
rows=list(rows.values()); now=int(time.time())
titled=sum(1 for r in rows if (r.get("title") or "").strip())
desc  =sum(1 for r in rows if (r.get("description") or "").strip())
live  =[r for r in rows if r.get("phase")=="primary" and (r.get("endTime") or 0)>now]
print(f"    distinct markets : {len(rows)}")
print(f"    with title       : {titled}")
print(f"    with description : {desc}")
print(f"    live (primary, not past endTime) : {len(live)}")
print(f"      of those, priced : {sum(1 for r in live if r.get('primaryYesPrice'))}")
print(f"      of those, volume>0 : {sum(1 for r in live if float(r.get('volumeUsdc') or 0)>0)}")
print(f"      closing within 7d  : {sum(1 for r in live if r['endTime']<=now+7*86400)}")
if rows:
    print("    VERDICT: TITLES FIXED." if titled==len(rows)
          else f"    VERDICT: PARTIAL — {len(rows)-titled} of {len(rows)} still untitled.")
    print("\n    sample of what is live now:")
    for r in sorted(live,key=lambda r:r["endTime"])[:5]:
        print(f"      [{r['category']:<8}] {(r.get('title') or r.get('description') or '(no text)')[:70]}")
PY
echo
echo "=============================================================="
