//! REST client for the any local API (default http://127.0.0.1:7001/v1).

use crate::model::{Identity, Message, Space};
use crate::sse::SseReader;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

/// The dataset holding a chat object's messages.
const DATASET_CHAT_MESSAGES: &str = "chat_messages";

/// Chat order is DAG order: `_ver.id` is stamped at creation and never moves
/// (edits bump `modifiedAt`, not this), and it's what the server pages by. A
/// descending window holds the *newest* N messages so arrivals enter it.
const SORT_NEWEST_FIRST: &str = "-_ver.id";

/// The `context.view` we stamp on outgoing messages: the sender's screen at
/// send time, which for this client is always the chat itself.
const CONTEXT_VIEW_CHAT: &str = "chat";

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

    /// One page of messages, returned oldest-first for display. `before` pages
    /// backwards through history: pass the oldest `_ver.id` you hold and the
    /// page ends just before it (a range filter, so a message arriving
    /// meanwhile can't shift the page the way `offset` would).
    pub async fn messages(
        &self,
        space_id: &str,
        object_id: &str,
        limit: usize,
        before: Option<&str>,
    ) -> Result<Vec<Message>> {
        let mut body = json!({
            "objectId": object_id,
            "dataset": DATASET_CHAT_MESSAGES,
            "sort": [SORT_NEWEST_FIRST],
            "limit": limit,
        });
        if let Some(b) = before {
            body["filter"] = json!({ "_ver.id": { "$lt": b } });
        }
        let v = self
            .post_json(&format!("/spaces/{space_id}/query"), body)
            .await?;
        let recs = v
            .get("records")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let mut msgs: Vec<Message> = recs.iter().filter_map(Message::from_record).collect();
        msgs.sort_by(Message::cmp_order);
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
        // Long records are indexed as several chunks, each its own hit with the
        // same (objectId, recordId) — a message must count once, so dedupe on
        // the record (keeping the best-ranked chunk's position).
        let mut seen = std::collections::HashSet::new();
        let hits = v
            .get("hits")
            .and_then(|h| h.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|h| {
                        // Hits from other datasets can't be chat messages.
                        if let Some(ds) = h.get("dataset").and_then(|d| d.as_str()) {
                            if ds != DATASET_CHAT_MESSAGES {
                                return None;
                            }
                        }
                        let hit = SearchHit {
                            object_id: h.get("objectId")?.as_str()?.to_string(),
                            record_id: h.get("recordId")?.as_str()?.to_string(),
                        };
                        seen.insert((hit.object_id.clone(), hit.record_id.clone()))
                            .then_some(hit)
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
                    "sort": [SORT_NEWEST_FIRST],
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
        // `context` is the sender's view at send time; agents reading the chat
        // resolve "here" from it. For a chat client "here" is the chat.
        let mut body = json!({
            "text": text,
            "context": {
                "spaceId": space_id,
                "objectId": object_id,
                "view": CONTEXT_VIEW_CHAT,
            },
        });
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

    /// Clears the unread reaction(s) on one message. A reaction is ordered
    /// after its target, so `mark_read` (which cuts at the message's own
    /// `_ver.id`) can never cover it. 204, idempotent, no-op without unread.
    pub async fn reactions_read(&self, space_id: &str, object_id: &str, msg_id: &str) -> Result<()> {
        let path =
            format!("/spaces/{space_id}/objects/{object_id}/chat/messages/{msg_id}/reactions-read");
        let resp = self.http.post(self.url(&path)).send().await?;
        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("reactions_read {code}: {}", first_line(&body));
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
            "sort": [SORT_NEWEST_FIRST],
            "limit": limit,
        });
        self.subscribe(&path, body).await
    }

    /// Live view of the account's space list (the tech-space `spaces` rows).
    /// Rows are raw tech-index records, not `SpaceInfo`, so this is used only
    /// as a change signal: any frame means "re-list `/spaces`".
    pub async fn subscribe_spaces(&self) -> Result<SseReader> {
        self.subscribe("/spaces/query/subscribe", json!({ "limit": 0 })).await
    }

    /// The type ids that declare the chat module in this space — the `owners`
    /// of the `chat_messages` dataset. Since 2026-09 the general chat is a
    /// bundle root that is its own type, and a declaring root hosts itself: the
    /// chat object *is* the owner id (its row carries the `__type__` marker in
    /// `any.type`, never the owner id). So these ids are matched against `id`.
    /// Empty when the space has no general chat installed yet (or on a
    /// pre-parts daemon, which has no such dataset entry).
    pub async fn chat_type_ids(&self, space_id: &str) -> Result<Vec<String>> {
        let v = self
            .get_json(&format!("/spaces/{space_id}/datasets"))
            .await?;
        Ok(v.get("datasets")
            .and_then(|d| d.as_array())
            .into_iter()
            .flatten()
            .filter(|d| d.get("name").and_then(|n| n.as_str()) == Some("chat_messages"))
            .filter_map(|d| d.get("owners").and_then(|o| o.as_array()))
            .flatten()
            .filter_map(|o| o.as_str().map(str::to_string))
            .collect())
    }

    /// Live view of a space's chat objects — drives unread badges and picks up
    /// chats created while we're running. `chat_types` comes from
    /// [`Api::chat_type_ids`] and is matched against `id` (the general-chat
    /// root is its own type and hosts itself). The filter is OR-ed with any
    /// object whose type layout is `chat`, so a chat installed after the
    /// owners were resolved still shows up live.
    pub async fn subscribe_chat_objects(
        &self,
        space_id: &str,
        chat_types: &[String],
    ) -> Result<SseReader> {
        let path = format!("/spaces/{space_id}/objects/query/subscribe");
        let body = json!({
            "filter": {"$or": [
                {"id": {"$in": chat_types}},
                {"type.layout.type": "chat"},
            ]},
            "sort": ["miniapp.pos"],
            "limit": 200,
        });
        self.subscribe(&path, body).await
    }

    /// The account event bus (any doc 21): ephemeral, at-most-once, no replay
    /// — `ready` on connect, then one `event` frame per envelope whose `type`
    /// is in `types` (exact, or a `prefix.*`). We listen for the serving
    /// anyrt's `bao.status` presence beats (anybao ADR-025), which any-ui's
    /// status bar reads the same way.
    pub async fn subscribe_events(&self, types: &[&str]) -> Result<SseReader> {
        let path = "/events/subscribe";
        // Types are dotted `[a-z0-9_.*]` slugs by the bus grammar, so the
        // query string needs no escaping.
        let mut url = format!("{}?scope=account", self.url(path));
        for t in types {
            url.push_str("&type=");
            url.push_str(t);
        }
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("subscribe {path}"))?;
        open_stream(resp, path).await
    }

    async fn subscribe(&self, path: &str, body: Value) -> Result<SseReader> {
        let resp = self
            .http
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("subscribe {path}"))?;
        open_stream(resp, path).await
    }
}

/// Failures before a stream opens are a normal JSON error envelope; after 200
/// everything arrives as frames.
async fn open_stream(resp: reqwest::Response, path: &str) -> Result<SseReader> {
    if !resp.status().is_success() {
        let code = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("subscribe {path} failed {code}: {}", first_line(&text));
    }
    Ok(SseReader::new(resp))
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
