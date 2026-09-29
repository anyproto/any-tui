//! REST client for the any local API (default http://127.0.0.1:7001/v1).

use crate::model::{Identity, Message, Space, SyncStatus};
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
/// enrichment (alongside creator/timestamp), so it isn't kept here.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub object_id: String,
    pub record_id: String,
    /// Fused rank score; higher is better. Comparable within one mode, which
    /// is what merging results across spaces relies on.
    pub score: f64,
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

/// The unsealed member view of a files-v2 file (`GET …/files/{fileId}`).
#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
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

    /// The identities directory. Rows are parsed one by one: a single odd
    /// row must not cost every other name (a whole-list parse did, once a
    /// `"spaceIds": null` appeared).
    pub async fn identities(&self) -> Result<Vec<Identity>> {
        let v = self.get_json("/identities").await?;
        Ok(v.get("identities")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|i| serde_json::from_value(i.clone()).ok()).collect())
            .unwrap_or_default())
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
    /// Chat-message search in one space. `object` narrows it to one chat on
    /// the server (`filter` is matched against the hit's host object row, so
    /// `{"id": chat}` keeps that chat's messages) — then `limit` counts that
    /// chat's hits, rather than the space's top N filtered afterwards.
    pub async fn search(
        &self,
        space_id: &str,
        query: &str,
        mode: &str,
        limit: usize,
        object: Option<&str>,
    ) -> Result<SearchResults> {
        let mut body = json!({
            "query": query,
            "scopes": ["chat"],
            "mode": mode,
            "limit": limit,
        });
        if let Some(o) = object {
            body["filter"] = json!({ "id": o });
        }
        let v = self
            .post_json(&format!("/spaces/{space_id}/search"), body)
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
                            score: h.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0),
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

    /// How many messages the chat holds (`includeTotal` on a 1-row query;
    /// `limit > 0` needs a `sort`).
    pub async fn message_count(&self, space_id: &str, object_id: &str) -> Result<u64> {
        let v = self
            .post_json(
                &format!("/spaces/{space_id}/query"),
                json!({
                    "objectId": object_id,
                    "dataset": DATASET_CHAT_MESSAGES,
                    "sort": [SORT_NEWEST_FIRST],
                    "limit": 1,
                    "includeTotal": true,
                }),
            )
            .await?;
        Ok(v.get("total").and_then(|t| t.as_u64()).unwrap_or(0))
    }

    /// Every message that carries an attachment or a web link, newest first —
    /// the source of the chat's file and link lists. Filtered server-side
    /// (`$regex` on the text, `$exists` on attachments) and paged by
    /// `_ver.id`, up to `cap` messages.
    pub async fn media_messages(&self, space_id: &str, object_id: &str, cap: usize) -> Result<Vec<Message>> {
        const PAGE: usize = 1000;
        let media = json!({ "$or": [
            { "text": { "$regex": "https?://" } },
            { "attachments": { "$exists": true } },
        ]});
        let mut out: Vec<Message> = Vec::new();
        loop {
            let filter = match out.last() {
                Some(m) if !m.ver.is_empty() => json!({ "$and": [media, { "_ver.id": { "$lt": m.ver } }] }),
                Some(_) => break,
                None => media.clone(),
            };
            let v = self
                .post_json(
                    &format!("/spaces/{space_id}/query"),
                    json!({
                        "objectId": object_id,
                        "dataset": DATASET_CHAT_MESSAGES,
                        "filter": filter,
                        "sort": [SORT_NEWEST_FIRST],
                        "limit": PAGE,
                    }),
                )
                .await?;
            let page: Vec<Message> = v
                .get("records")
                .and_then(|r| r.as_array())
                .map(|a| a.iter().filter_map(Message::from_record).collect())
                .unwrap_or_default();
            let done = page.len() < PAGE;
            out.extend(page);
            if done || out.len() >= cap {
                break;
            }
        }
        out.truncate(cap);
        Ok(out)
    }

    /// Edges from any space this device holds that point at one of the
    /// chat's messages: `(source space, source object, source record, target
    /// record)`. The account-wide read — the per-object backlinks route only
    /// sees edges from the chat's own space, and whispers live in DMs.
    pub async fn message_backlinks(&self, space_id: &str, chat_id: &str) -> Result<Vec<(String, String, String, String)>> {
        let target = format!("any://o/{space_id}/{chat_id}");
        let v = self
            .get_json(&format!("/backlinks?kind=link&target={}", url_encode(&target)))
            .await?;
        let mut out = Vec::new();
        for sp in v.get("spaces").and_then(|s| s.as_array()).into_iter().flatten() {
            for e in sp.get("parts").and_then(|p| p.as_array()).into_iter().flatten() {
                let src = |k: &str| e.pointer(&format!("/source/{k}")).and_then(|x| x.as_str());
                let tgt = e.pointer("/target/recordId").and_then(|x| x.as_str());
                if let (Some(s), Some(o), Some(r), Some(t)) = (src("spaceId"), src("objectId"), src("recordId"), tgt)
                    && src("dataset") == Some(DATASET_CHAT_MESSAGES)
                {
                    out.push((s.to_string(), o.to_string(), r.to_string(), t.to_string()));
                }
            }
        }
        Ok(out)
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

    /// Rewrites one of your messages (`s/old/new/`). Edits bump `modifiedAt`,
    /// never `_ver.id`, so the message keeps its place.
    pub async fn edit_message(&self, space_id: &str, object_id: &str, msg_id: &str, text: &str) -> Result<()> {
        let path = format!("/spaces/{space_id}/objects/{object_id}/chat/messages/{msg_id}");
        let resp = self
            .http
            .patch(self.url(&path))
            .json(&json!({ "text": text }))
            .send()
            .await
            .with_context(|| format!("PATCH {path}"))?;
        json_or_err(resp, &path).await?;
        Ok(())
    }

    /// Opens (derives + activates) the 1-1 space with `identity`; idempotent,
    /// and both peers land on the same space id. Returns it.
    pub async fn one_to_one(&self, identity: &str) -> Result<String> {
        let v = self
            .post_json("/spaces/one-to-one", json!({ "otherIdentity": identity }))
            .await?;
        str_field(&v, "id")
    }

    /// Member identities of a space (for a DM: you and the peer).
    pub async fn members(&self, space_id: &str) -> Result<Vec<String>> {
        let v = self.get_json(&format!("/spaces/{space_id}/members")).await?;
        Ok(v.get("members")
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("identity").and_then(|i| i.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Requests waiting on you: 1-1 rows not approved yet
    /// (`one_to_one_pending`) and spaces someone added you to
    /// (`invite_pending`). `status` on each row says which.
    pub async fn pending_dms(&self) -> Result<Vec<Space>> {
        let mut out = Vec::new();
        for status in ["one_to_one_pending", "invite_pending"] {
            let v = self.get_json(&format!("/spaces?status={status}")).await?;
            let arr = v.get("spaces").cloned().unwrap_or(Value::Array(vec![]));
            out.extend(serde_json::from_value::<Vec<Space>>(arr)?);
        }
        Ok(out)
    }

    /// Accepts a direct-add invite to a space (materializes it here).
    pub async fn accept_invite(&self, space_id: &str) -> Result<()> {
        self.post_json(&format!("/spaces/{space_id}/invite/accept"), json!({}))
            .await?;
        Ok(())
    }

    pub async fn accept_dm(&self, space_id: &str) -> Result<()> {
        self.post_json(&format!("/spaces/{space_id}/one-to-one/accept"), json!({}))
            .await?;
        Ok(())
    }

    /// The space's general chat, installing it if missing (adopt-or-install,
    /// idempotent; a derived root, so both sides of a 1-1 meet on one object).
    /// We only call this for DMs we open — never on an ordinary space.
    pub async fn general_chat(&self, space_id: &str) -> Result<String> {
        let v = self
            .post_json("/catalog/general-chat/setup", json!({ "spaceId": space_id }))
            .await?;
        v.pointer("/bundles/0/bundle/rootId")
            .and_then(|r| r.as_str())
            .map(str::to_string)
            .context("general-chat setup: no rootId")
    }

    /// Creates the account-scoped local (device-only, never synced)
    /// collection `name`; idempotent. See any docs/26-local-store.md.
    pub async fn local_ensure(&self, name: &str) -> Result<()> {
        let resp = self
            .http
            .put(self.url("/local/collections"))
            .json(&json!({ "scope": "account", "name": name }))
            .send()
            .await
            .context("PUT /local/collections")?;
        json_or_err(resp, "/local/collections").await?;
        Ok(())
    }

    /// One local document by id; `None` when it doesn't exist yet.
    pub async fn local_get(&self, coll: &str, id: &str) -> Result<Option<Value>> {
        let body = json!({ "coll": { "scope": "account", "name": coll }, "id": id });
        match self.post_json("/local/get", body).await {
            Ok(v) => Ok(v.get("record").cloned()),
            Err(e) if e.to_string().contains("local.doc_not_found") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Writes one local document (it must carry `id`), replacing any old one.
    pub async fn local_put(&self, coll: &str, doc: Value) -> Result<()> {
        let body = json!({ "coll": { "scope": "account", "name": coll }, "docs": [doc] });
        self.post_json("/local/upsert", body).await?;
        Ok(())
    }

    pub async fn file_info(&self, space_id: &str, file_id: &str) -> Result<FileInfo> {
        let v = self.get_json(&format!("/spaces/{space_id}/files/{file_id}")).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// The file's plaintext as a streaming response. Bytes not yet local are
    /// fetched on demand (from a LAN peer or the network's read base);
    /// `409 file.not_available` when neither can serve them yet.
    pub async fn file_content(&self, space_id: &str, file_id: &str) -> Result<reqwest::Response> {
        let path = format!("/spaces/{space_id}/files/{file_id}/content");
        let resp = self
            .http
            .get(self.url(&path))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        if !resp.status().is_success() {
            // Reuse the error-envelope formatting; it always bails here.
            json_or_err(resp, &path).await?;
            bail!("{path}: unexpected status");
        }
        Ok(resp)
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
    /// Renames the account. `PUT /account/metadata` is full-replace, so the
    /// stored profile (`GET /account` → `metadata`) is read first and its
    /// description and icon are sent back unchanged. If that read fails
    /// nothing is written: replacing a profile we couldn't see could wipe it.
    pub async fn rename(&self, name: &str) -> Result<()> {
        let account = self.get_json("/account").await.context("read the current profile")?;
        let mut body = json!({ "name": name });
        if let Some(meta) = account.get("metadata") {
            for key in ["description", "iconCid"] {
                if let Some(v) = meta.get(key).and_then(|v| v.as_str()).filter(|v| !v.is_empty()) {
                    body[key] = json!(v);
                }
            }
        }
        let resp = self
            .http
            .put(self.url("/account/metadata"))
            .json(&body)
            .send()
            .await
            .context("PUT /account/metadata")?;
        json_or_err(resp, "/account/metadata").await?;
        Ok(())
    }

    /// The account's device registry, with this device and the elected
    /// active device per app.
    pub async fn devices(&self) -> Result<crate::model::Devices> {
        Ok(crate::model::Devices::from_json(&self.get_json("/devices").await?))
    }

    /// Both p2p layers' peers (`GET /debug/p2p`): who is connected, over
    /// what, for which spaces, and whether it's one of your own devices.
    pub async fn debug_p2p(&self) -> Result<Value> {
        self.get_json("/debug/p2p").await
    }

    pub async fn sync_status(&self, space_id: &str) -> Result<SyncStatus> {
        let v = self.get_json(&format!("/spaces/{space_id}/sync-status")).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Account-wide sync-status flips: `ready`, then sparse `status` frames
    /// (no snapshot — read the GET first), `lagged` when some were dropped.
    pub async fn subscribe_sync_status(&self) -> Result<SseReader> {
        let path = "/sync-status/subscribe";
        let resp = self
            .http
            .get(self.url(path))
            .send()
            .await
            .with_context(|| format!("subscribe {path}"))?;
        open_stream(resp, path).await
    }

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

/// Percent-encodes a query value (everything but the unreserved set).
fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn str_field(v: &Value, key: &str) -> Result<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .with_context(|| format!("response has no `{key}`"))
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(200).collect()
}
