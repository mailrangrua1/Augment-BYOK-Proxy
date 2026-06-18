// Thread store: lưu history theo conversation_id để khôi phục khi VSIX mới
// gửi chat_history rỗng (Augment >=0.859 dựa vào server-side thread state).
//
// Mỗi conversation_id giữ một danh sách AugmentChatHistory. Khi turn mới đến
// với chat_history rỗng nhưng có conversation_id đã thấy trước, prepend từ
// store. Sau khi stream upstream hoàn tất, ghi turn mới (request + response)
// vào store và persist xuống disk (best-effort, không block client).

use std::{collections::HashMap, path::Path};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::protocol::{
  AugmentChatHistory, NodeIn, NodeOut, ThinkingNode, ToolUse, REQUEST_NODE_TEXT,
  REQUEST_NODE_TOOL_RESULT, RESPONSE_NODE_MAIN_TEXT_FINISHED, RESPONSE_NODE_RAW_RESPONSE,
  RESPONSE_NODE_THINKING, RESPONSE_NODE_TOOL_USE,
};

/// Tối đa số exchange giữ trong store cho mỗi conversation. Tránh phình vô hạn
/// khi user chạy task dài. Vẫn đủ để model có context vài chục turn gần nhất;
/// trên ngưỡng này history_summary sẽ tự kích hoạt nén head.
const MAX_EXCHANGES_PER_CONVERSATION: usize = 200;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ThreadStore {
  #[serde(default)]
  pub threads: HashMap<String, ThreadEntry>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ThreadEntry {
  /// History các exchange đã hoàn tất (oldest → newest).
  #[serde(default)]
  pub history: Vec<AugmentChatHistory>,
  /// Mốc cập nhật cuối (epoch ms). Phục vụ TTL/eviction tương lai.
  #[serde(default)]
  pub updated_at_ms: u64,
}

impl ThreadStore {
  pub async fn load_from_file(path: &Path) -> anyhow::Result<Self> {
    let bytes = match tokio::fs::read(path).await {
      Ok(v) => v,
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
      Err(err) => {
        return Err(err)
          .with_context(|| format!("读取 thread store 失败: {}", path.display()))
      }
    };
    if bytes.is_empty() {
      return Ok(Self::default());
    }
    let parsed: Self = serde_json::from_slice(&bytes).context("解析 thread store JSON 失败")?;
    Ok(parsed)
  }

  pub async fn save_to_file(&self, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
      tokio::fs::create_dir_all(parent)
        .await
        .with_context(|| format!("创建 thread store 目录失败: {}", parent.display()))?;
    }
    let json = serde_json::to_vec(self).context("序列化 thread store JSON 失败")?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, json)
      .await
      .with_context(|| format!("写入临时 thread store 失败: {}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
      .await
      .with_context(|| format!("重命名 thread store 失败: {}", path.display()))?;
    Ok(())
  }

  pub fn get(&self, conversation_id: &str) -> Option<&ThreadEntry> {
    self.threads.get(conversation_id)
  }

  /// Append 1 exchange (request + response) vào tail.
  /// Trả về true nếu store thay đổi.
  pub fn append_exchange(
    &mut self,
    conversation_id: &str,
    exchange: AugmentChatHistory,
    now_ms: u64,
  ) -> bool {
    if conversation_id.trim().is_empty() {
      return false;
    }
    if exchange.request_id.trim().is_empty() {
      // Không append exchange thiếu request_id (history_summary boundary cần nó).
      return false;
    }
    let entry = self
      .threads
      .entry(conversation_id.to_string())
      .or_default();

    // Idempotency: nếu request_id đã tồn tại ở tail, replace (turn retried).
    if let Some(last) = entry.history.last_mut() {
      if last.request_id == exchange.request_id {
        *last = exchange;
        entry.updated_at_ms = now_ms;
        return true;
      }
    }
    entry.history.push(exchange);
    entry.updated_at_ms = now_ms;

    // Bound size — trim từ đầu nếu quá lớn.
    let len = entry.history.len();
    if len > MAX_EXCHANGES_PER_CONVERSATION {
      let drop = len - MAX_EXCHANGES_PER_CONVERSATION;
      entry.history.drain(0..drop);
    }
    true
  }

  pub fn remove_conversation(&mut self, conversation_id: &str) -> bool {
    let key = conversation_id.trim();
    if key.is_empty() {
      return false;
    }
    self.threads.remove(key).is_some()
  }

  #[allow(dead_code)]
  pub fn clear_all(&mut self) {
    self.threads.clear();
  }
}

/// Chuyển NodeOut (server emits) → NodeIn (client format) để lưu vào history
/// dưới dạng response_nodes. AugmentChatHistory dùng NodeIn cho cả request và
/// response.
fn node_out_to_node_in(out: &NodeOut) -> NodeIn {
  NodeIn {
    id: out.id,
    node_type: out.node_type,
    content: out.content.clone(),
    text_node: None,
    tool_result_node: None,
    image_node: None,
    image_id_node: None,
    ide_state_node: None,
    edit_events_node: None,
    checkpoint_ref_node: None,
    change_personality_node: None,
    file_node: None,
    file_id_node: None,
    history_summary_node: None,
    tool_use: out.tool_use.clone(),
    thinking: out.thinking.as_ref().map(|t| ThinkingNode {
      summary: t.summary.clone(),
    }),
  }
}

/// Snapshot 1 turn để lưu vào thread store.
#[derive(Debug, Default)]
pub struct TurnSnapshot {
  /// request_id của turn (lấy từ X-Request-Id, conversation_id+timestamp,
  /// hoặc UUID generated).
  pub request_id: String,
  /// User message text (từ augment.message hoặc text node hiện tại).
  pub request_message: String,
  /// Request nodes "snapshot" (tool_results, text nodes hiện tại đã merged).
  pub request_nodes: Vec<NodeIn>,
  /// Response text (full_text streamed từ assistant).
  pub response_text: String,
  /// Response nodes (tool_use, thinking, main_text_finished).
  pub response_nodes: Vec<NodeIn>,
}

impl TurnSnapshot {
  pub fn new(request_id: String, request_message: String) -> Self {
    Self {
      request_id,
      request_message,
      request_nodes: Vec::new(),
      response_nodes: Vec::new(),
      response_text: String::new(),
    }
  }

  #[allow(dead_code)]
  pub fn add_request_text(&mut self, content: String) {
    if content.trim().is_empty() {
      return;
    }
    let id = -50_000 - (self.request_nodes.len() as i32);
    self.request_nodes.push(NodeIn {
      id,
      node_type: REQUEST_NODE_TEXT,
      content: String::new(),
      text_node: Some(crate::protocol::TextNode { content }),
      tool_result_node: None,
      image_node: None,
      image_id_node: None,
      ide_state_node: None,
      edit_events_node: None,
      checkpoint_ref_node: None,
      change_personality_node: None,
      file_node: None,
      file_id_node: None,
      history_summary_node: None,
      tool_use: None,
      thinking: None,
    });
  }

  /// Copy tool_result và text nodes từ request hiện tại để lưu cùng turn.
  pub fn snapshot_request_nodes(&mut self, source: &[NodeIn]) {
    for n in source {
      match n.node_type {
        REQUEST_NODE_TOOL_RESULT | REQUEST_NODE_TEXT => {
          self.request_nodes.push(n.clone());
        }
        _ => {}
      }
    }
  }

  pub fn append_text_delta(&mut self, delta: &str) {
    self.response_text.push_str(delta);
  }

  pub fn add_thinking(&mut self, summary: String) {
    if summary.trim().is_empty() {
      return;
    }
    let id = -60_000 - (self.response_nodes.len() as i32);
    self.response_nodes.push(NodeIn {
      id,
      node_type: RESPONSE_NODE_THINKING,
      content: String::new(),
      text_node: None,
      tool_result_node: None,
      image_node: None,
      image_id_node: None,
      ide_state_node: None,
      edit_events_node: None,
      checkpoint_ref_node: None,
      change_personality_node: None,
      file_node: None,
      file_id_node: None,
      history_summary_node: None,
      tool_use: None,
      thinking: Some(ThinkingNode { summary }),
    });
  }

  pub fn add_tool_use(&mut self, tool_use: ToolUse) {
    let id = -60_000 - (self.response_nodes.len() as i32);
    self.response_nodes.push(NodeIn {
      id,
      node_type: RESPONSE_NODE_TOOL_USE,
      content: String::new(),
      text_node: None,
      tool_result_node: None,
      image_node: None,
      image_id_node: None,
      ide_state_node: None,
      edit_events_node: None,
      checkpoint_ref_node: None,
      change_personality_node: None,
      file_node: None,
      file_id_node: None,
      history_summary_node: None,
      tool_use: Some(tool_use),
      thinking: None,
    });
  }

  /// Gọi sau khi stream finalize: thêm 1 MAIN_TEXT_FINISHED vào response_nodes.
  pub fn finalize_response_nodes(&mut self) {
    if self.response_text.trim().is_empty() {
      return;
    }
    let id = -60_000 - (self.response_nodes.len() as i32);
    self.response_nodes.push(NodeIn {
      id,
      node_type: RESPONSE_NODE_MAIN_TEXT_FINISHED,
      content: self.response_text.clone(),
      text_node: None,
      tool_result_node: None,
      image_node: None,
      image_id_node: None,
      ide_state_node: None,
      edit_events_node: None,
      checkpoint_ref_node: None,
      change_personality_node: None,
      file_node: None,
      file_id_node: None,
      history_summary_node: None,
      tool_use: None,
      thinking: None,
    });
  }

  pub fn into_chat_history(mut self) -> AugmentChatHistory {
    self.finalize_response_nodes();
    AugmentChatHistory {
      response_text: self.response_text,
      request_message: self.request_message,
      request_id: self.request_id,
      request_nodes: self.request_nodes,
      structured_request_nodes: Vec::new(),
      nodes: Vec::new(),
      response_nodes: self.response_nodes,
      structured_output_nodes: Vec::new(),
    }
  }
}

/// Convert NodeOut từ stream thành signal cho TurnSnapshot. Gọi mỗi khi
/// chat_stream emit 1 chunk về client.
pub fn ingest_node_out(snap: &mut TurnSnapshot, out: &NodeOut) {
  match out.node_type {
    RESPONSE_NODE_RAW_RESPONSE => {
      // Đã handle qua append_text_delta ở caller (từ text field).
      let _ = node_out_to_node_in;
    }
    RESPONSE_NODE_THINKING => {
      if let Some(t) = out.thinking.as_ref() {
        snap.add_thinking(t.summary.clone());
      }
    }
    RESPONSE_NODE_TOOL_USE => {
      if let Some(tu) = out.tool_use.as_ref() {
        snap.add_tool_use(tu.clone());
      }
    }
    _ => {}
  }
}

/// Type alias dễ đọc trong AppState.
pub type SharedThreadStore = std::sync::Arc<RwLock<ThreadStore>>;
