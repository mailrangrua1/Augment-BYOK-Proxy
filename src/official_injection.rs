use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::Value;
use tracing::{debug, warn};

use crate::config::Config;
use crate::protocol::{AugmentBlobs, AugmentRequest, NodeIn, TextNode, REQUEST_NODE_TEXT};
use crate::util::{join_url, normalize_raw_token, now_ms};
use crate::AppState;

const OFFICIAL_CODEBASE_RETRIEVAL_MAX_OUTPUT_LENGTH: i64 = 20000;
const OFFICIAL_CODEBASE_RETRIEVAL_TIMEOUT_MS: u64 = 12_000;
const OFFICIAL_CONTEXT_CANVAS_TIMEOUT_MS: u64 = 4_000;
const CONTEXT_CANVAS_CACHE_TTL_MS: u64 = 5 * 60_000;

#[derive(Debug, Default)]
pub(crate) struct ContextCanvasCache {
  by_base_url: HashMap<String, ContextCanvasCacheEntry>,
}

#[derive(Debug, Clone)]
struct ContextCanvasCacheEntry {
  expires_at_ms: u64,
  by_id: HashMap<String, ContextCanvas>,
}

#[derive(Debug, Clone)]
struct ContextCanvas {
  id: String,
  name: String,
  description: String,
}

#[derive(Debug, Clone)]
struct ExternalSource {
  id: String,
  title: String,
  url: String,
  source_type: String,
  snippet: String,
}

pub(crate) async fn maybe_inject_official_context(
  state: &AppState,
  cfg: &Config,
  req: &mut AugmentRequest,
  hard_timeout: Duration,
) {
  let hard_timeout_ms = dur_ms_or(hard_timeout, 120_000);
  maybe_inject_official_codebase_retrieval(state, cfg, req, hard_timeout_ms).await;
  maybe_inject_official_context_canvas(state, cfg, req, hard_timeout_ms).await;
  maybe_inject_official_external_sources(state, cfg, req, hard_timeout_ms).await;
}

fn dur_ms_or(d: Duration, fallback_ms: u64) -> u64 {
  let ms = d.as_millis();
  if ms == 0 {
    fallback_ms
  } else {
    ms.min(u64::MAX as u128) as u64
  }
}

fn normalize_string(s: &str) -> String {
  s.trim().to_string()
}

fn truncate_text(s: &str, max_chars: usize) -> String {
  let text = s.trim();
  if text.is_empty() {
    return String::new();
  }
  if text.chars().count() <= max_chars {
    return text.to_string();
  }
  let mut out = String::new();
  for (i, ch) in text.chars().enumerate() {
    if i >= max_chars {
      break;
    }
    out.push(ch);
  }
  out.trim_end().to_string() + "…"
}

fn make_text_request_node(id: i32, text: &str) -> NodeIn {
  NodeIn {
    id,
    node_type: REQUEST_NODE_TEXT,
    content: String::new(),
    text_node: Some(TextNode {
      content: text.to_string(),
    }),
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
  }
}

fn pick_injection_target(req: &mut AugmentRequest) -> &mut Vec<NodeIn> {
  if !req.request_nodes.is_empty() {
    return &mut req.request_nodes;
  }
  if !req.structured_request_nodes.is_empty() {
    return &mut req.structured_request_nodes;
  }
  &mut req.nodes
}

fn build_inline_code_context_text(req: &AugmentRequest) -> String {
  if req.disable_selected_code_details {
    return String::new();
  }
  let code = format!("{}{}{}", req.prefix, req.selected_code, req.suffix);
  normalize_string(&code)
}

fn build_user_extra_text_parts(req: &AugmentRequest, has_nodes: bool) -> Vec<String> {
  if has_nodes {
    return Vec::new();
  }
  if req.message_source.trim() == "prompt" {
    return Vec::new();
  }
  if req.disable_selected_code_details {
    return Vec::new();
  }
  let main = req.message.trim();
  let mut out: Vec<String> = Vec::new();
  let code = build_inline_code_context_text(req);
  if !code.is_empty() && code != main {
    out.push(code);
  }
  let diff = normalize_string(&req.diff);
  if !diff.is_empty() && diff != main && diff != out.first().map(String::as_str).unwrap_or("") {
    out.push(diff);
  }
  out
}

fn build_codebase_retrieval_information_request(req: &AugmentRequest) -> String {
  let mut parts: Vec<String> = Vec::new();
  let main = normalize_string(&req.message);
  if !main.is_empty() {
    parts.push(main);
  }
  for p in build_user_extra_text_parts(req, false) {
    let s = normalize_string(&p);
    if !s.is_empty() {
      parts.push(s);
    }
  }
  if !req.path.trim().is_empty() {
    parts.push(format!("path: {}", req.path.trim()));
  }
  if !req.lang.trim().is_empty() {
    parts.push(format!("lang: {}", req.lang.trim()));
  }
  parts.join("\n\n").trim().to_string()
}

#[derive(Debug, Clone, Default)]
struct NormalizedBlobs {
  checkpoint_id: Option<String>,
  added_blobs: Vec<String>,
  deleted_blobs: Vec<String>,
}

fn normalize_blobs(raw: Option<&AugmentBlobs>) -> NormalizedBlobs {
  let Some(b) = raw else {
    return NormalizedBlobs::default();
  };
  let checkpoint_id = b
    .checkpoint_id
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_string);
  let added_blobs = normalize_string_list(&b.added_blobs, 500);
  let deleted_blobs = normalize_string_list(&b.deleted_blobs, 500);
  NormalizedBlobs {
    checkpoint_id,
    added_blobs,
    deleted_blobs,
  }
}

fn normalize_string_list(raw: &[String], max_items: usize) -> Vec<String> {
  let mut out: Vec<String> = Vec::new();
  let mut seen: HashSet<String> = HashSet::new();
  for v in raw {
    let s = normalize_string(v);
    if s.is_empty() || seen.contains(&s) {
      continue;
    }
    seen.insert(s.clone());
    out.push(s);
    if out.len() >= max_items {
      break;
    }
  }
  out
}

fn merge_unique_limit(a: &[String], b: &[String], max_items: usize) -> Vec<String> {
  let mut out: Vec<String> = Vec::new();
  let mut seen: HashSet<String> = HashSet::new();
  for v in a.iter().chain(b.iter()) {
    let s = normalize_string(v);
    if s.is_empty() || seen.contains(&s) {
      continue;
    }
    seen.insert(s.clone());
    out.push(s);
    if out.len() >= max_items {
      break;
    }
  }
  out
}

async fn maybe_inject_official_codebase_retrieval(
  state: &AppState,
  cfg: &Config,
  req: &mut AugmentRequest,
  hard_timeout_ms: u64,
) -> bool {
  if req.disable_retrieval {
    return false;
  }

  let info = build_codebase_retrieval_information_request(req);
  if info.is_empty() {
    return false;
  }

  let completion_url = cfg.official.base_url.trim();
  let api_token = normalize_raw_token(&cfg.official.api_token);
  if completion_url.is_empty() || api_token.is_empty() {
    debug!("officialRetrieval skipped: missing completion_url/api_token");
    return false;
  }

  let t_ms = (hard_timeout_ms / 2)
    .min(OFFICIAL_CODEBASE_RETRIEVAL_TIMEOUT_MS)
    .max(2000);
  let timeout = Duration::from_millis(t_ms);

  let base_blobs = normalize_blobs(req.blobs.as_ref());
  let user_guided = normalize_string_list(&req.user_guided_blobs, 500);

  let has_checkpoint = base_blobs
    .checkpoint_id
    .as_deref()
    .is_some_and(|s| !s.is_empty());
  let has_added = !base_blobs.added_blobs.is_empty();
  let has_deleted = !base_blobs.deleted_blobs.is_empty();
  let has_user_guided = !user_guided.is_empty();
  if !has_checkpoint && !has_added && !has_deleted && !has_user_guided {
    return false;
  }

  let added_blobs = merge_unique_limit(&base_blobs.added_blobs, &user_guided, 500);
  let payload_base = serde_json::json!({
    "information_request": info,
    "blobs": {
      "checkpoint_id": base_blobs.checkpoint_id,
      "added_blobs": added_blobs,
      "deleted_blobs": base_blobs.deleted_blobs,
    },
    "dialog": [],
    "max_output_length": OFFICIAL_CODEBASE_RETRIEVAL_MAX_OUTPUT_LENGTH,
  });
  let payload = serde_json::json!({
    "information_request": payload_base["information_request"],
    "blobs": payload_base["blobs"],
    "dialog": payload_base["dialog"],
    "max_output_length": payload_base["max_output_length"],
    "disable_codebase_retrieval": false,
    "enable_commit_retrieval": false,
  });

  match fetch_official_codebase_retrieval(
    state,
    completion_url,
    &api_token,
    payload,
    payload_base,
    timeout,
  )
  .await
  {
    Ok(formatted) => {
      let formatted = formatted.trim();
      if formatted.is_empty() {
        return false;
      }
      let node = make_text_request_node(-20, formatted);
      let target = pick_injection_target(req);
      target.push(node);
      debug!(
        chars = formatted.len(),
        target_len = target.len(),
        "officialRetrieval injected"
      );
      true
    }
    Err(err) => {
      warn!(error=%err, "officialRetrieval failed (ignored)");
      false
    }
  }
}

async fn fetch_official_codebase_retrieval(
  state: &AppState,
  completion_url: &str,
  api_token: &str,
  payload: Value,
  payload_base: Value,
  timeout: Duration,
) -> anyhow::Result<String> {
  let url = join_url(completion_url, "agents/codebase-retrieval")?;
  let mut resp = post_official_json(state, &url, api_token, &payload, timeout).await?;
  if !resp.status().is_success() {
    let status = resp.status().as_u16();
    if status == 400 || status == 422 {
      resp = post_official_json(state, &url, api_token, &payload_base, timeout).await?;
    }
  }

  if !resp.status().is_success() {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    anyhow::bail!(
      "agents/codebase-retrieval {}: {}",
      status.as_u16(),
      truncate_text(&text, 300)
    );
  }

  let json: Value = resp.json().await.unwrap_or(Value::Null);
  let formatted = json
    .get("formatted_retrieval")
    .and_then(|v| v.as_str())
    .or_else(|| json.get("formattedRetrieval").and_then(|v| v.as_str()))
    .unwrap_or("")
    .trim()
    .to_string();
  Ok(formatted)
}

async fn post_official_json(
  state: &AppState,
  url: &str,
  api_token: &str,
  payload: &Value,
  timeout: Duration,
) -> anyhow::Result<reqwest::Response> {
  let resp = state
    .http
    .post(url)
    .timeout(timeout)
    .header("content-type", "application/json")
    .bearer_auth(api_token)
    .json(payload)
    .send()
    .await?;
  Ok(resp)
}

async fn maybe_inject_official_context_canvas(
  state: &AppState,
  cfg: &Config,
  req: &mut AugmentRequest,
  hard_timeout_ms: u64,
) -> bool {
  if req.disable_retrieval {
    return false;
  }

  let canvas_id = req.canvas_id.trim().to_string();
  if canvas_id.is_empty() {
    return false;
  }

  let completion_url = cfg.official.base_url.trim();
  let api_token = normalize_raw_token(&cfg.official.api_token);
  if completion_url.is_empty() || api_token.is_empty() {
    debug!("officialContextCanvas skipped: missing completion_url/api_token");
    return false;
  }

  if let Some(canvas) = get_canvas_from_cache(state, completion_url, &canvas_id).await {
    return inject_context_canvas_node(req, canvas, &canvas_id);
  }

  let t_ms = (hard_timeout_ms.saturating_mul(15) / 100)
    .min(OFFICIAL_CONTEXT_CANVAS_TIMEOUT_MS)
    .max(800);
  let deadline = now_ms().saturating_add(t_ms);

  let mut page_token = String::new();
  let mut pages: u64 = 0;
  while pages < 3 && now_ms().saturating_add(200) < deadline {
    pages += 1;
    let remaining = deadline.saturating_sub(now_ms()).max(300);
    let timeout = Duration::from_millis(remaining);
    match fetch_official_context_canvas_list(
      state,
      completion_url,
      &api_token,
      100,
      page_token.as_str(),
      timeout,
    )
    .await
    {
      Ok(raw) => {
        let (canvases, next_page_token) = normalize_official_context_canvas_list_response(&raw);
        if !canvases.is_empty() {
          upsert_canvas_cache(state, completion_url, canvases).await;
        }
        if let Some(canvas) = get_canvas_from_cache(state, completion_url, &canvas_id).await {
          return inject_context_canvas_node(req, canvas, &canvas_id);
        }
        if next_page_token.is_empty() {
          break;
        }
        page_token = next_page_token;
      }
      Err(err) => {
        debug!(error=%err, "officialContextCanvas list failed (ignored)");
        break;
      }
    }
  }

  false
}

async fn fetch_official_context_canvas_list(
  state: &AppState,
  completion_url: &str,
  api_token: &str,
  page_size: u64,
  page_token: &str,
  timeout: Duration,
) -> anyhow::Result<Value> {
  let url = join_url(completion_url, "context-canvas/list")?;
  let payload = serde_json::json!({
    "page_size": page_size,
    "page_token": page_token,
  });

  let resp = state
    .http
    .post(url)
    .timeout(timeout)
    .header("content-type", "application/json")
    .bearer_auth(api_token)
    .json(&payload)
    .send()
    .await?;

  if !resp.status().is_success() {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    anyhow::bail!(
      "context-canvas/list {}: {}",
      status.as_u16(),
      truncate_text(&text, 300)
    );
  }
  let json: Value = resp.json().await.unwrap_or(Value::Null);
  Ok(json)
}

fn normalize_official_context_canvas_list_response(raw: &Value) -> (Vec<ContextCanvas>, String) {
  let mut list: Vec<Value> = Vec::new();
  let obj = raw.as_object();
  if let Some(arr) = raw.as_array() {
    list.extend(arr.iter().cloned());
  } else if let Some(obj) = obj {
    if let Some(arr) = obj.get("canvases").and_then(|v| v.as_array()) {
      list.extend(arr.iter().cloned());
    }
  }

  let mut canvases: Vec<ContextCanvas> = Vec::new();
  for it in list {
    let Some(o) = it.as_object() else { continue };
    let id = get_string_any(o, &["canvas_id", "canvasId", "canvasID", "id"]);
    let name = get_string_any(o, &["name", "title"]);
    let description = get_string_any(o, &["description", "summary"]);
    if id.is_empty() && name.is_empty() && description.is_empty() {
      continue;
    }
    canvases.push(ContextCanvas {
      id,
      name,
      description,
    });
  }

  let next_page_token = obj
    .and_then(|o| {
      Some(get_string_any(
        o,
        &[
          "next_page_token",
          "nextPageToken",
          "next_pageToken",
          "page_token",
          "pageToken",
        ],
      ))
    })
    .unwrap_or_default();

  (canvases, next_page_token)
}

fn format_context_canvas_for_prompt(canvas: &ContextCanvas, canvas_id: &str) -> String {
  let id = if !canvas_id.trim().is_empty() {
    canvas_id.trim().to_string()
  } else {
    normalize_string(&canvas.id)
  };
  let name = truncate_text(&canvas.name, 200);
  let description = truncate_text(&canvas.description, 4000);
  let mut lines: Vec<String> = vec!["[CONTEXT_CANVAS]".to_string()];
  if !id.is_empty() {
    lines.push(format!("canvas_id={id}"));
  }
  if !name.is_empty() {
    lines.push(format!("name={name}"));
  }
  if !description.is_empty() {
    lines.push(format!("description={description}"));
  }
  if lines.len() == 1 {
    return String::new();
  }
  lines.push("[/CONTEXT_CANVAS]".to_string());
  lines.join("\n").trim().to_string()
}

fn inject_context_canvas_node(
  req: &mut AugmentRequest,
  canvas: ContextCanvas,
  canvas_id: &str,
) -> bool {
  let text = format_context_canvas_for_prompt(&canvas, canvas_id);
  if text.is_empty() {
    return false;
  }

  let target = pick_injection_target(req);
  let node = make_text_request_node(-22, &text);
  if let Some(idx) = target.iter().position(|n| n.id == -20) {
    target.insert(idx, node);
  } else {
    target.push(node);
  }
  debug!(
    chars = text.len(),
    target_len = target.len(),
    "officialContextCanvas injected"
  );
  true
}

async fn get_canvas_from_cache(
  state: &AppState,
  completion_url: &str,
  canvas_id: &str,
) -> Option<ContextCanvas> {
  let key = normalize_base_url_key(completion_url);
  if key.is_empty() || canvas_id.trim().is_empty() {
    return None;
  }

  let now = now_ms();
  {
    let mut cache = state.context_canvas_cache.write().await;
    let entry = cache.by_base_url.get(&key).cloned();
    let Some(entry) = entry else { return None };
    if entry.expires_at_ms <= now {
      cache.by_base_url.remove(&key);
      return None;
    }
    return entry.by_id.get(canvas_id.trim()).cloned();
  }
}

async fn upsert_canvas_cache(state: &AppState, completion_url: &str, canvases: Vec<ContextCanvas>) {
  let key = normalize_base_url_key(completion_url);
  if key.is_empty() {
    return;
  }

  let now = now_ms();
  let expires_at_ms = now.saturating_add(CONTEXT_CANVAS_CACHE_TTL_MS);
  let mut cache = state.context_canvas_cache.write().await;
  let entry = cache
    .by_base_url
    .entry(key)
    .or_insert_with(|| ContextCanvasCacheEntry {
      expires_at_ms,
      by_id: HashMap::new(),
    });
  for c in canvases {
    if c.id.trim().is_empty() {
      continue;
    }
    entry.by_id.insert(c.id.trim().to_string(), c);
  }
  entry.expires_at_ms = expires_at_ms;
}

fn normalize_base_url_key(base_url: &str) -> String {
  let mut s = base_url.trim().to_string();
  if s.is_empty() {
    return s;
  }
  if !s.ends_with('/') {
    s.push('/');
  }
  s
}

fn get_string_any(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
  for k in keys {
    if let Some(v) = obj.get(*k) {
      let out = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
      };
      let out = out.trim().to_string();
      if !out.is_empty() {
        return out;
      }
    }
  }
  String::new()
}

async fn maybe_inject_official_external_sources(
  state: &AppState,
  cfg: &Config,
  req: &mut AugmentRequest,
  hard_timeout_ms: u64,
) -> bool {
  if req.disable_retrieval {
    return false;
  }

  let msg = req.message.trim();
  if msg.is_empty() {
    return false;
  }

  let explicit_ids = normalize_string_list(&req.external_source_ids, 200);
  let should_auto = !req.disable_auto_external_sources;
  if explicit_ids.is_empty() && !should_auto {
    return false;
  }

  let completion_url = cfg.official.base_url.trim();
  let api_token = normalize_raw_token(&cfg.official.api_token);
  if completion_url.is_empty() || api_token.is_empty() {
    debug!("officialExternalSources skipped: missing completion_url/api_token");
    return false;
  }

  let t_ms = (hard_timeout_ms / 4).min(8000).max(1500);
  let implicit_timeout_ms = ((t_ms as f64) * 0.4) as u64;
  let implicit_timeout_ms = implicit_timeout_ms.min(3500).max(1000);

  let mut wanted_ids = explicit_ids.clone();
  if wanted_ids.is_empty() && should_auto {
    let implicit_timeout = Duration::from_millis(implicit_timeout_ms);
    match fetch_official_implicit_external_sources(
      state,
      completion_url,
      &api_token,
      msg,
      implicit_timeout,
    )
    .await
    {
      Ok(raw) => {
        let implicit_ids = normalize_external_source_ids_from_implicit_result(&raw);
        if !implicit_ids.is_empty() {
          wanted_ids = implicit_ids;
        }
      }
      Err(err) => {
        debug!(error=%err, "officialExternalSources implicit failed (ignored)");
      }
    }
  }
  if wanted_ids.is_empty() && should_auto {
    return false;
  }

  let search_timeout_ms = if !explicit_ids.is_empty() {
    t_ms
  } else {
    t_ms.saturating_sub(implicit_timeout_ms).max(1500)
  };
  let search_timeout = Duration::from_millis(search_timeout_ms);

  let raw = match fetch_official_search_external_sources(
    state,
    completion_url,
    &api_token,
    msg,
    search_timeout,
  )
  .await
  {
    Ok(v) => v,
    Err(err) => {
      warn!(error=%err, "officialExternalSources failed (ignored)");
      return false;
    }
  };

  let results = normalize_official_external_sources_search_results(&raw);
  if results.is_empty() {
    return false;
  }

  let wanted_set: HashSet<String> = wanted_ids.iter().cloned().collect();
  let filtered: Vec<ExternalSource> = results
    .iter()
    .cloned()
    .filter(|r| !r.id.is_empty() && wanted_set.contains(&r.id))
    .collect();
  let chosen: Vec<ExternalSource> = (if !filtered.is_empty() {
    filtered
  } else {
    results
  })
  .into_iter()
  .take(6)
  .collect();

  let text = format_external_sources_for_prompt(&chosen, &wanted_ids);
  if text.is_empty() {
    return false;
  }

  let target = pick_injection_target(req);
  let node = make_text_request_node(-21, &text);
  if let Some(idx) = target.iter().position(|n| n.id == -20) {
    target.insert(idx, node);
  } else {
    target.push(node);
  }
  debug!(
    chars = text.len(),
    target_len = target.len(),
    "officialExternalSources injected"
  );
  true
}

async fn fetch_official_implicit_external_sources(
  state: &AppState,
  completion_url: &str,
  api_token: &str,
  message: &str,
  timeout: Duration,
) -> anyhow::Result<Value> {
  let url = join_url(completion_url, "get-implicit-external-sources")?;
  let payload = serde_json::json!({ "message": message });
  let resp = state
    .http
    .post(url)
    .timeout(timeout)
    .header("content-type", "application/json")
    .bearer_auth(api_token)
    .json(&payload)
    .send()
    .await?;

  if !resp.status().is_success() {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    anyhow::bail!(
      "get-implicit-external-sources {}: {}",
      status.as_u16(),
      truncate_text(&text, 300)
    );
  }
  Ok(resp.json().await.unwrap_or(Value::Null))
}

fn normalize_external_source_ids_from_implicit_result(raw: &Value) -> Vec<String> {
  let mut out: Vec<Value> = Vec::new();
  if let Some(arr) = raw.as_array() {
    out.extend(arr.iter().cloned());
  } else if let Some(obj) = raw.as_object() {
    let candidates = [
      "external_source_ids",
      "externalSourceIds",
      "source_ids",
      "sourceIds",
      "implicit_external_source_ids",
      "implicitExternalSourceIds",
      "external_sources",
      "externalSources",
      "sources",
      "implicit_external_sources",
      "implicitExternalSources",
    ];
    for k in candidates {
      if let Some(arr) = obj.get(k).and_then(|v| v.as_array()) {
        out.extend(arr.iter().cloned());
        break;
      }
    }
  }

  let mut ids: Vec<String> = Vec::new();
  for it in out {
    match it {
      Value::String(s) => ids.push(s),
      Value::Object(o) => {
        let cand = get_string_any(
          &o,
          &[
            "id",
            "source_id",
            "sourceId",
            "external_source_id",
            "externalSourceId",
            "externalSourceID",
          ],
        );
        if !cand.is_empty() {
          ids.push(cand);
        }
      }
      _ => {}
    }
  }

  normalize_string_list(&ids, 200)
}

async fn fetch_official_search_external_sources(
  state: &AppState,
  completion_url: &str,
  api_token: &str,
  query: &str,
  timeout: Duration,
) -> anyhow::Result<Value> {
  let url = join_url(completion_url, "search-external-sources")?;
  let payload = serde_json::json!({
    "query": query,
    "source_types": [],
  });
  let resp = state
    .http
    .post(url)
    .timeout(timeout)
    .header("content-type", "application/json")
    .bearer_auth(api_token)
    .json(&payload)
    .send()
    .await?;

  if !resp.status().is_success() {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    anyhow::bail!(
      "search-external-sources {}: {}",
      status.as_u16(),
      truncate_text(&text, 300)
    );
  }
  Ok(resp.json().await.unwrap_or(Value::Null))
}

fn normalize_official_external_sources_search_results(raw: &Value) -> Vec<ExternalSource> {
  let mut list: Vec<Value> = Vec::new();
  if let Some(arr) = raw.as_array() {
    list.extend(arr.iter().cloned());
  } else if let Some(obj) = raw.as_object() {
    let candidates = [
      "sources",
      "external_sources",
      "externalSources",
      "items",
      "results",
    ];
    for k in candidates {
      if let Some(arr) = obj.get(k).and_then(|v| v.as_array()) {
        list.extend(arr.iter().cloned());
        break;
      }
    }
  }

  let mut out: Vec<ExternalSource> = Vec::new();
  for it in list {
    match it {
      Value::String(s) => {
        let snippet = truncate_text(&s, 2000);
        if !snippet.is_empty() {
          out.push(ExternalSource {
            id: String::new(),
            title: String::new(),
            url: String::new(),
            source_type: String::new(),
            snippet,
          });
        }
      }
      Value::Object(o) => {
        let id = get_string_any(
          &o,
          &[
            "id",
            "source_id",
            "sourceId",
            "external_source_id",
            "externalSourceId",
            "externalSourceID",
          ],
        );
        let title = get_string_any(
          &o,
          &[
            "title",
            "name",
            "display_name",
            "displayName",
            "source_title",
            "sourceTitle",
          ],
        );
        let url = get_string_any(&o, &["url", "href", "link", "source_url", "sourceUrl"]);
        let source_type = get_string_any(&o, &["source_type", "sourceType", "type", "kind"]);
        let snippet = get_string_any(
          &o,
          &["snippet", "summary", "excerpt", "text", "content", "body"],
        );
        let snippet = truncate_text(&snippet, 4000);
        if id.is_empty() && title.is_empty() && url.is_empty() && snippet.is_empty() {
          continue;
        }
        out.push(ExternalSource {
          id,
          title,
          url,
          source_type,
          snippet,
        });
      }
      _ => {}
    }
  }
  out
}

fn format_external_sources_for_prompt(
  results: &[ExternalSource],
  selected_external_source_ids: &[String],
) -> String {
  let mut lines: Vec<String> = vec!["[EXTERNAL_SOURCES]".to_string()];
  if !selected_external_source_ids.is_empty() {
    lines.push(format!(
      "selected_external_source_ids={}",
      selected_external_source_ids.join(",")
    ));
  }
  for r in results {
    let title = normalize_string(&r.title);
    let url = normalize_string(&r.url);
    let id = normalize_string(&r.id);
    let source_type = normalize_string(&r.source_type);
    let snippet = truncate_text(&r.snippet, 4000);

    let mut header_parts: Vec<String> = Vec::new();
    if !title.is_empty() {
      header_parts.push(title);
    }
    if !source_type.is_empty() {
      header_parts.push(format!("type={source_type}"));
    }
    if !url.is_empty() {
      header_parts.push(url);
    } else if !id.is_empty() {
      header_parts.push(format!("id={id}"));
    }
    if header_parts.is_empty() && snippet.is_empty() {
      continue;
    }
    lines.push(format!(
      "- {}",
      if header_parts.is_empty() {
        "(source)".to_string()
      } else {
        header_parts.join(" | ")
      }
    ));
    if !snippet.is_empty() {
      lines.push(snippet);
    }
  }
  if lines.len() == 1 {
    return String::new();
  }
  lines.push("[/EXTERNAL_SOURCES]".to_string());
  lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn normalize_external_source_ids_from_implicit_result_variants() {
    let v = json!({ "externalSourceIds": ["a", "b", "a", ""] });
    assert_eq!(
      normalize_external_source_ids_from_implicit_result(&v),
      vec!["a".to_string(), "b".to_string()]
    );

    let v = json!([{ "sourceId": "x" }, { "externalSourceID": "y" }]);
    assert_eq!(
      normalize_external_source_ids_from_implicit_result(&v),
      vec!["x".to_string(), "y".to_string()]
    );
  }

  #[test]
  fn normalize_official_context_canvas_list_response_variants() {
    let v = json!({
      "canvases": [
        { "canvasId": "c1", "name": "N", "description": "D" },
        { "id": "c2", "title": "T", "summary": "S" }
      ],
      "nextPageToken": "p2"
    });
    let (canvases, next) = normalize_official_context_canvas_list_response(&v);
    assert_eq!(next, "p2".to_string());
    assert_eq!(canvases.len(), 2);
    assert_eq!(canvases[0].id, "c1");
    assert_eq!(canvases[1].id, "c2");
  }
}
