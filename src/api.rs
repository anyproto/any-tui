//! REST client for the any local API (default http://127.0.0.1:7001/v1).

use crate::model::{Identity, Message, Space};
use crate::sse::SseReader;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

/// The dataset holding a chat object's messages.
const DATASET_CHAT_MESSAGES: &str = "chat_messages";

#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    #[serde(default)]
    pub version: String,
    /// The logged-in account address — this is "me".
    #[serde(default)]
    pub account: String,
}

/// One raw search hit. For a chat-scope hit, `object_id` is the chat and
/// `record_id` is the message id. The matched text is fetched during
/// enrichment (alongside creator/timestamp), so it isn't kept here. The wire
/// also carries a `score`, but results are re-sorted by time, so we drop it.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub object_id: String,
    pub record_id: String,
}

/// A search response: ranked hits plus what the engine actually did. `mode` is
/// the mode that ran (hybrid can degrade to fts) and `vector_status` says
/// whether semantic recall participated (used | unavailable | disabled | skipped).
#[derive(Debug, Clone)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub mode: String,
    pub vector_status: String,
}

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    base: String,
}

impl Api {
    pub fn new(base: &str) -> Result<Api> {
        Ok(Api {
            // No global timeout: SSE responses are long-lived by design.
            http: reqwest::Client::builder()
                .build()
                .context("build http client")?,
            base: base.trim_end_matches('/').to_string(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let resp = self
            .http
            .get(self.url(path))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        json_or_err(resp, path).await
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self
            .http
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        json_or_err(resp, path).await
    }

    pub async fn health(&self) -> Result<Health> {
        let v = self.get_json("/health").await?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn identities(&self) -> Result<Vec<Identity>> {
        let v = self.get_json("/identities").await?;
        let arr = v
            .get("identities")
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        Ok(serde_json::from_value(arr).unwrap_or_default())
    }

    pub async fn spaces(&self) -> Result<Vec<Space>> {
        let v = self.get_json("/spaces").await?;
        let arr = v.get("spaces").cloned().unwrap_or(Value::Array(vec![]));
        let spaces: Vec<Space> = serde_json::from_value(arr)?;
        Ok(spaces
            .into_iter()
            .filter(|s| s.status.is_empty() || s.status == "active")
            .collect())
    }

    /// One page of messages, returned oldest-first for display.
    /// `offset` pages backwards through history (newest-first under the hood).
    pub async fn messages(
        &self,
        space_id: &str,
        object_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Message>> {
        let v = self
            .post_json(
                &format!("/spaces/{space_id}/query"),
                json!({
                    "objectId": object_id,
                    "dataset": DATASET_CHAT_MESSAGES,
                    "sort": ["-createdAt"],
                    "limit": limit,
                    "offset": offset,
                }),
            )
            .await?;
        let recs = v
            .get("records")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let mut msgs: Vec<Message> = recs.iter().filter_map(Message::from_record).collect();
        msgs.sort_by(|a, b| {
            a.created_at
                .partial_cmp(&b.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        });
        Ok(msgs)
    }

    /// Full-text / semantic search over chat messages in one space. `mode` is
    /// "hybrid" | "fts" | "vector"; we always restrict to the "chat" scope so
    /// only messages come back (never pages or object names).
    pub async fn search(
        &self,
        space_id: &str,
        query: &str,
        mode: &str,
        limit: usize,
    ) -> Result<SearchResults> {
        let v = self
            .post_json(
                &format!("/spaces/{space_id}/search"),
                json!({
                    "query": query,
                    "scopes": ["chat"],
                    "mode": mode,
                    "limit": limit,
                }),
            )
            .await?;
        let hits = v
            .get("hits")
            .and_then(|h| h.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| {
                        Some(SearchHit {
                            object_id: h.get("objectId")?.as_str()?.to_string(),
                            record_id: h.get("recordId")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(SearchResults {
            hits,
            mode: v
                .get("mode")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
            vector_status: v
                .get("vectorStatus")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }

    /// Fetches specific chat messages by id. Search hits carry only the message
    /// id and text, so this backfills creator/timestamp for the results view.
    pub async fn messages_by_ids(
        &self,
        space_id: &str,
        object_id: &str,
        ids: &[String],
    ) -> Result<Vec<Message>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let v = self
            .post_json(
                &format!("/spaces/{space_id}/query"),
                json!({
                    "objectId": object_id,
                    "dataset": DATASET_CHAT_MESSAGES,
                    "filter": { "id": { "$in": ids } },
                    "sort": ["-createdAt"],
                    "limit": ids.len(),
                }),
            )
            .await?;
        let recs = v
            .get("records")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(recs.iter().filter_map(Message::from_record).collect())
    }

    pub async fn send(
        &self,
        space_id: &str,
        object_id: &str,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<()> {
        let mut body = json!({ "text": text });
        if let Some(r) = reply_to {
            body["replyToMessageId"] = json!(r);
        }
        self.post_json(
            &format!("/spaces/{space_id}/objects/{object_id}/chat/messages"),
            body,
        )
        .await?;
        Ok(())
    }

    /// Marks `msg_id` and everything before it read. Returns 204 (no body).
    pub async fn mark_read(&self, space_id: &str, object_id: &str, msg_id: &str) -> Result<()> {
        let path = format!("/spaces/{space_id}/objects/{object_id}/chat/messages/{msg_id}/read");
        let resp = self.http.post(self.url(&path)).send().await?;
        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("mark_read {code}: {}", first_line(&body));
        }
        Ok(())
    }

    /// Live window over a chat's messages, newest-first, `limit` records wide.
    pub async fn subscribe_messages(
        &self,
        space_id: &str,
        object_id: &str,
        limit: usize,
    ) -> Result<SseReader> {
        let path = format!("/spaces/{space_id}/query/subscribe");
        let body = json!({
            "objectId": object_id,
            "dataset": DATASET_CHAT_MESSAGES,
            "sort": ["-createdAt"],
            "limit": limit,
        });
        self.subscribe(&path, body).await
    }

    /// Live view of a space's chat objects — drives unread badges and picks up
    /// chats created while we're running.
    pub async fn subscribe_chat_objects(&self, space_id: &str) -> Result<SseReader> {
        let path = format!("/spaces/{space_id}/objects/query/subscribe");
        let body = json!({
            "filter": {"any.types": "chat"},
            "sort": ["nav.pos"],
            "limit": 200,
        });
        self.subscribe(&path, body).await
    }

    async fn subscribe(&self, path: &str, body: Value) -> Result<SseReader> {
        let resp = self
            .http
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("subscribe {path}"))?;
        // Failures before the stream opens are a normal JSON error envelope;
        // after 200 everything arrives as frames.
        if !resp.status().is_success() {
            let code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("subscribe {path} failed {code}: {}", first_line(&text));
        }
        Ok(SseReader::new(resp))
    }
}

async fn json_or_err(resp: reqwest::Response, path: &str) -> Result<Value> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // Error envelope: {"error":{"code":"...","message":"..."}}
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let code = v
                .pointer("/error/code")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            let msg = v
                .pointer("/error/message")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if !msg.is_empty() {
                bail!("{path} {status}: {code} {msg}");
            }
        }
        bail!("{path} {status}: {}", first_line(&text));
    }
    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("{path}: bad json"))
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(200).collect()
}
