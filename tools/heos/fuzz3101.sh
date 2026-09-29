#!/bin/bash
# fuzz3101 — campaign driver for the asm prober.
# Single target, delays between probes, stop-on-wedge.
# Usage: fuzz3101 <ip> <paths-file>
PROBE=/tmp/probe
IP=${1:?usage: fuzz3101 <ip> <paths-file>}
PATHS=${2:?usage: fuzz3101 <ip> <paths-file>}
PORT=3101
TIMEOUT_MS=2500
DELAY=0.4
WEDGE_LIMIT=3

KNOWN_404='The resource to /'
wedge=0
n=0
hits=0

# Build a GET request file per path (avoids quoting issues)
req() { printf 'GET /%s HTTP/1.0\r\nHost: x\r\n\r\n' "$1" > /tmp/fuzz/req.txt; }

echo "=== fuzz3101 vs $IP:$PORT ($(wc -l < "$PATHS") paths) ==="
while IFS= read -r p; do
  [ -z "$p" ] && continue
  n=$((n+1))
  req "$p"
  out=$(timeout 10 $PROBE "$IP" "$PORT" /tmp/fuzz/req.txt $TIMEOUT_MS 2>&1)
  code=$?
  if [ $code -eq 2 ]; then
    echo "[$n] /$p  CONNECT-FAIL — target down?"
    wedge=$((wedge+1))
  elif [ $code -eq 1 ]; then
    echo "[$n] /$p  TIMEOUT"
    wedge=$((wedge+1))
  else
    wedge=0
    body=$(echo "$out" | tail -1)
    if echo "$body" | grep -q "$KNOWN_404"; then
      : # expected 404, quiet
    else
      hits=$((hits+1))
      echo "[$n] /$p  HIT: $(echo "$body" | head -c 150)"
    fi
  fi
  if [ "$wedge" -ge "$WEDGE_LIMIT" ]; then
    echo "!!! WEDGE DETECTED ($wedge consecutive failures) — aborting, target may be wedged"
    exit 3
  fi
  sleep $DELAY
done < "$PATHS"
echo "=== done: $n probed, $hits hits ==="
