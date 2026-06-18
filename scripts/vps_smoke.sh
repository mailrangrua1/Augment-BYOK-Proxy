#!/bin/bash
set -u
cd /home/ubuntu/augment-byok-proxy
AUTH=$(awk -F': *' '/^  auth_token:/ {gsub(/^"|"$/,"",$2); print $2; exit}' config.yaml)
echo "auth len=${#AUTH}"

echo "=== A. Probe (no upstream) ==="
curl -s -o /tmp/r.body -w 'HTTP %{http_code} bytes=%{size_download}\n' \
  -X POST 'http://127.0.0.1:8317/chat-stream' \
  -H "x-api-key: ${AUTH}" -H 'x-byok-mode: byok' -H 'content-type: application/json' \
  --data '{}'
head -c 400 /tmp/r.body; echo

echo "=== B. Hello chat ==="
curl -sN --max-time 30 \
  -X POST 'http://127.0.0.1:8317/chat-stream' \
  -H "x-api-key: ${AUTH}" -H 'x-byok-mode: byok' -H 'content-type: application/json' \
  --data '{"message":"reply with single word OK","tool_definitions":[]}' \
  | head -c 1200
echo
echo "=== C. Last 30 log lines ==="
journalctl -u byok-proxy.service --no-pager -n 30 2>&1 | tail -30
