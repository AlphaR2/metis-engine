#!/usr/bin/env bash
# Uploads a market cover image to Panta's image host (Cloudinary) and prints the link to use as the market's image.
#   PANTA_API_KEY=... devnet/panta-upload-image.sh path/to/cover.png
# Panta recommends a 1024x1024 square. The image bytes go straight to Cloudinary; Panta only signs the upload.
# The key is read from your environment and is never printed. Run it on your own machine.
set -euo pipefail
FILE=${1:?path to a PNG/JPG/WebP cover image}
: "${PANTA_API_KEY:?set PANTA_API_KEY in your shell (export PANTA_API_KEY=...), not on the command line history}"
BASE=${PANTA_BASE_URL:-https://live-api.panta.market/api/v1}
[ -f "$FILE" ] || { echo "no such file: $FILE"; exit 1; }
SIGN=$(curl -sS -X POST "$BASE/markets/create/image-upload/" -H "X-Api-Key: $PANTA_API_KEY" -H 'Content-Type: application/json' -d '{}')
ARGS=$(python3 -I -c '
import json,sys
d=json.loads(sys.stdin.read())
if "uploadUrl" not in d: sys.exit("Panta did not return an upload form: "+json.dumps(d)[:300])
print(d["uploadUrl"])
for k,v in d["fields"].items(): print("%s=%s"%(k,v))
' <<<"$SIGN")
URL=$(printf '%s\n' "$ARGS" | head -1)
FORM=(); while IFS= read -r line; do [ -n "$line" ] && FORM+=(-F "$line"); done < <(printf '%s\n' "$ARGS" | tail -n +2)
RESP=$(curl -sS -X POST "$URL" "${FORM[@]}" -F "file=@$FILE")
python3 -I -c '
import json,sys
d=json.loads(sys.stdin.read())
if "secure_url" not in d: sys.exit("upload failed: "+json.dumps(d)[:300])
print("Cover image link (paste it into the Cover image field):"); print(d["secure_url"])
' <<<"$RESP"
