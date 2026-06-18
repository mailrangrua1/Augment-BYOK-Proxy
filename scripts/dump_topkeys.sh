#!/bin/bash
# Extract top-level keys & sizes of recent /chat-stream requests from journalctl.
set -u
SINCE="${1:-1 hour ago}"
TMP=$(mktemp)
sudo journalctl -u byok-proxy.service --no-pager --since "$SINCE" -o cat 2>&1 \
  | grep -oE '"chat-stream 请求 len=[0-9]+ body=\{.*' \
  > "$TMP" || true
N=$(wc -l < "$TMP")
echo "matched lines: $N"
echo
i=0
while IFS= read -r line; do
  i=$((i+1))
  [ $i -gt 5 ] && break
  echo "=== sample $i ==="
  body=$(printf '%s' "$line" | sed -E 's/^[^=]*body=//')
  echo "$body" | python3 -c '
import json, sys
raw = sys.stdin.read()
try:
    obj = json.loads(raw)
except Exception as e:
    print("PARSE_FAIL", e); print(raw[:200]); sys.exit(0)
print("TOP_KEYS:", list(obj.keys()))
ch = obj.get("chat_history") or obj.get("chatHistory") or []
print("chat_history.len:", len(ch) if isinstance(ch, list) else type(ch).__name__)
if isinstance(ch, list) and ch:
    last = ch[-1]
    if isinstance(last, dict):
        print("last_chat_history.req_msg_chars:", len((last.get("request_message") or last.get("requestMessage") or "")))
        print("last_chat_history.resp_text_chars:", len((last.get("response_text") or last.get("responseText") or "")))
print("turn_id:", obj.get("turn_id") or obj.get("turnId"))
print("conversation_id:", obj.get("conversation_id") or obj.get("conversationId"))
print("message_chars:", len(obj.get("message") or obj.get("text") or ""))
print("nodes.len:", len(obj.get("nodes") or []))
print("structured_request_nodes.len:", len(obj.get("structured_request_nodes") or obj.get("structuredRequestNodes") or []))
print("request_nodes.len:", len(obj.get("request_nodes") or obj.get("requestNodes") or []))
print("system_prompt_replacements:", obj.get("system_prompt_replacements") or obj.get("systemPromptReplacements"))
print("third_party_override:", obj.get("third_party_override") or obj.get("thirdPartyOverride"))
print("user_guidelines.len:", len(obj.get("user_guidelines") or obj.get("userGuidelines") or ""))
print("workspace_guidelines.len:", len(obj.get("workspace_guidelines") or obj.get("workspaceGuidelines") or ""))
print("agent_memories.len:", len(obj.get("agent_memories") or obj.get("agentMemories") or ""))
print("tool_definitions:", obj.get("tool_definitions") or obj.get("toolDefinitions"))
'
  echo
done < "$TMP"
rm -f "$TMP"
