#!/bin/bash
# Inspect chat-stream behavior (history_summary, n_history sizes, errors).
set -u
SINCE="${1:-1 hour ago}"

echo "=== A. byok-proxy ALL since '$SINCE' (compact) ==="
sudo journalctl -u byok-proxy.service --no-pager --since "$SINCE" 2>&1 \
  | grep -E "history_summary|chat_history|conversation_id|trigger|chat-stream|上游|error|warn" \
  | grep -vE "DEBUG dump|byok.inject_official|reuse idle|pooling idle|connecting to|connected to|starting new connection" \
  | tail -120
echo
echo "=== B. last 5 chat-stream req sizes (search dump_body INFO chat-stream 请求) ==="
sudo journalctl -u byok-proxy.service --no-pager --since "$SINCE" 2>&1 \
  | grep -E "INFO chat-stream 请求 len=" \
  | tail -10 | sed -E 's/^.*chat-stream 请求 len=([0-9]+).*$/req_bytes=\1/'
echo
echo "=== C. summary trigger lines ==="
sudo journalctl -u byok-proxy.service --no-pager --since "$SINCE" 2>&1 \
  | grep -E "history_summary 触发|history_summary 已在|history_summary 命中|history_summary 使用|history_summary 摘要" | tail -30
echo
echo "=== D. errors / warns ==="
sudo journalctl -u byok-proxy.service --no-pager --since "$SINCE" 2>&1 \
  | grep -E "ERROR|WARN|error_response|未授权|❌" | tail -30
echo
echo "=== E. gcli2api (kiro) and kiro provider hits ==="
sudo journalctl -u byok-proxy.service --no-pager --since "$SINCE" 2>&1 \
  | grep -iE "aiclient2api|kiro" | tail -10
echo "=== F. gcli2api service errors / restarts ==="
sudo journalctl -u gcli2api.service --no-pager --since "$SINCE" 2>&1 | tail -30
