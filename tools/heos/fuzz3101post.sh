#!/bin/bash
# POST variant: many embedded APIs only answer POST
PROBE=/tmp/probe
IP=${1:?usage}
PORT=3101
wedge=0; n=0; hits=0
while IFS= read -r p; do
  [ -z "$p" ] && continue
  n=$((n+1))
  { printf 'POST /%s HTTP/1.0\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 20\r\n\r\n{"command":"status"}' "$p"; } > /tmp/fuzz/req.txt
  out=$(timeout 10 $PROBE "$IP" "$PORT" /tmp/fuzz/req.txt 2500 2>&1)
  code=$?
  if [ $code -ne 0 ]; then wedge=$((wedge+1)); else wedge=0; fi
  if [ "$wedge" -ge 3 ]; then echo "!!! WEDGE — aborting"; exit 3; fi
  body=$(echo "$out" | tail -1)
  if [ -n "$body" ] && ! echo "$body" | grep -q 'Resource not found'; then
    hits=$((hits+1)); echo "[$n] POST /$p  HIT: $(echo "$body" | head -c 150)"
  fi
  sleep 0.4
done < /tmp/fuzz/paths.txt
echo "=== done: $n probed, $hits hits ==="
