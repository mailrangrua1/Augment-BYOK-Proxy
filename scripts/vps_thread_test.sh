#!/bin/bash
# Test thread_store: gửi 2 turn cùng conversation_id, turn 2 chỉ gửi message ngắn
# (chat_history rỗng); xác minh model "nhớ" từ turn 1.
set -u
cd /home/ubuntu/augment-byok-proxy
AUTH=$(awk -F': *' '/^  auth_token:/ {gsub(/^"|"$/,"",$2); print $2; exit}' config.yaml)
CONV="thread-test-$(date +%s)"
echo "auth_len=${#AUTH} conv=$CONV"

echo "=== Turn 1: tell secret word ==="
curl -sN --max-time 60 -X POST 'http://127.0.0.1:8317/chat-stream' \
  -H "x-api-key: ${AUTH}" -H 'x-byok-mode: byok' -H 'content-type: application/json' \
  --data "$(cat <<EOF
{"message":"My favorite color is purple-77 and my favorite number is 4321. Acknowledge with 'noted'.","conversation_id":"$CONV","tool_definitions":[]}
EOF
)" | tail -3
echo
sleep 2
echo "=== Inspect thread_store after turn 1 ==="
ls -la thread_store.json 2>&1 | head -1
python3 -c "
import json, sys
try:
    d = json.load(open('thread_store.json'))
except FileNotFoundError:
    print('no thread_store.json yet'); sys.exit(0)
ts = d.get('threads') or {}
print('threads:', list(ts.keys()))
ent = ts.get('$CONV')
if ent:
    print('history.len:', len(ent.get('history') or []))
    if ent['history']:
        h0 = ent['history'][0]
        print('exchange0.request_id:', h0.get('request_id'))
        print('exchange0.request_message:', (h0.get('request_message') or '')[:80])
        print('exchange0.response_text:', (h0.get('response_text') or '')[:120])
        print('exchange0.response_nodes.len:', len(h0.get('response_nodes') or []))
"
echo
echo "=== Turn 2: ask the secret word back (no chat_history) ==="
curl -sN --max-time 60 -X POST 'http://127.0.0.1:8317/chat-stream' \
  -H "x-api-key: ${AUTH}" -H 'x-byok-mode: byok' -H 'content-type: application/json' \
  --data "$(cat <<EOF
{"message":"What favorite color and favorite number did I tell you earlier? Reply concisely.","conversation_id":"$CONV","tool_definitions":[]}
EOF
)" | tail -8
echo
echo "=== Last 30 log lines (filtered) ==="
sudo journalctl -u byok-proxy.service --no-pager --since '2 minutes ago' 2>&1 \
  | grep -E "thread store|chat-stream 请求 len=|conversation_id=$CONV" | tail -30
