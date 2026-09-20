//! Minimal SSE reader for the any API subscribe streams.
//!
//! Wire contract (verified against internal/server/handlers_query_subscribe.go):
//!   event: ready    data: {}
//!   event: snapshot data: {"records":[...],"total":n,"hasNext":b}
//!   event: changes  data: [ {versionId, added[], updated[], removed[]} ]   <- an ARRAY
//!   event: closed   data: {"reason":"..."}                                 <- terminal
//!   : keepalive                                                            <- comment, every 25s
//!
//! The event bus (`GET /events/subscribe`, any doc 21) shares the framing but
//! has no snapshot: `ready`, then `event` frames carrying one envelope each.
//!
//! There is no unsubscribe: dropping the response ends the subscription.

use anyhow::{Context, Result};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::Value;
use std::pin::Pin;

#[derive(Debug, Clone)]
pub enum Frame {
    Ready,
    Snapshot(Vec<Value>),
    Changes(Vec<Change>),
    Closed(String),
    /// One event-bus envelope, verbatim (`{type, scope, target?, data, sender}`).
    Event(Value),
    /// Unknown event names are passed through rather than treated as errors:
    /// the frame set is documented as additive.
    Other(#[allow(dead_code)] String),
}

#[derive(Debug, Clone, Default)]
pub struct Change {
    /// Full documents, with `id` guaranteed present.
    pub added: Vec<Value>,
    pub updated: Vec<Value>,
    /// (id, reason) — reason is one of deleted | filtered-out | displaced | unknown.
    /// Only "deleted" means the record is really gone.
    pub removed: Vec<(String, String)>,
}

pub struct SseReader {
    stream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    buf: Vec<u8>,
    done: bool,
}

impl SseReader {
    pub fn new(resp: reqwest::Response) -> Self {
        Self {
            stream: Box::pin(resp.bytes_stream()),
            buf: Vec::new(),
            done: false,
        }
    }

    /// Returns the next frame, or None when the stream ends.
    pub async fn next_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            if let Some(block) = self.take_block() {
                match parse_block(&block)? {
                    Some(f) => return Ok(Some(f)),
                    // Comment-only / empty block (keepalive): keep reading.
                    None => continue,
                }
            }
            if self.done {
                return Ok(None);
            }
            match self.stream.next().await {
                Some(chunk) => self.buf.extend_from_slice(&chunk.context("sse read")?),
                None => {
                    self.done = true;
                    if self.buf.is_empty() {
                        return Ok(None);
                    }
                }
            }
        }
    }

    /// Pops one complete `\n\n`-terminated block off the buffer.
    fn take_block(&mut self) -> Option<String> {
        let pos = self.buf.windows(2).position(|w| w == b"\n\n")?;
        let block = self.buf.drain(..pos + 2).collect::<Vec<u8>>();
        Some(String::from_utf8_lossy(&block).into_owned())
    }
}

fn parse_block(block: &str) -> Result<Option<Frame>> {
    let mut event = String::new();
    let mut data = String::new();
    for line in block.lines() {
        // Comments (": keepalive") and blank lines carry no fields.
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => event = value.to_string(),
            "data" => {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            _ => {}
        }
    }
    if event.is_empty() {
        return Ok(None);
    }

    let frame = match event.as_str() {
        "ready" => Frame::Ready,
        "snapshot" => {
            let v: Value = serde_json::from_str(&data).context("snapshot json")?;
            let records = v
                .get("records")
                .and_then(|r| r.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                // snapshot records may contain literal nulls
                .filter(|r| !r.is_null())
                .collect();
            Frame::Snapshot(records)
        }
        "changes" => {
            let v: Value = serde_json::from_str(&data).context("changes json")?;
            let arr = v.as_array().cloned().unwrap_or_default();
            Frame::Changes(arr.iter().map(parse_change).collect())
        }
        "event" => {
            let v: Value = serde_json::from_str(&data).context("event json")?;
            Frame::Event(v)
        }
        "closed" => {
            let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
            Frame::Closed(
                v.get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
            )
        }
        other => Frame::Other(other.to_string()),
    };
    Ok(Some(frame))
}

fn parse_change(v: &Value) -> Change {
    Change {
        added: records(v, "added"),
        updated: records(v, "updated"),
        removed: v
            .get("removed")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| {
                        // Tolerate the stale docs shape (bare id strings) as well
                        // as the real {id, reason} objects.
                        if let Some(s) = r.as_str() {
                            return Some((s.to_string(), "unknown".to_string()));
                        }
                        let id = r.get("id")?.as_str()?.to_string();
                        let reason = r
                            .get("reason")
                            .and_then(|x| x.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        Some((id, reason))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Extracts `doc` out of each {id, doc, ops} record, making sure `id` is present
/// inside the doc so downstream code can treat it like a snapshot record.
fn records(v: &Value, key: &str) -> Vec<Value> {
    v.get(key)
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let id = r.get("id").and_then(|i| i.as_str())?.to_string();
                    let mut doc = r.get("doc").cloned().unwrap_or(Value::Null);
                    if doc.is_null() {
                        return None;
                    }
                    if let Some(obj) = doc.as_object_mut() {
                        obj.entry("id").or_insert(Value::String(id));
                    }
                    Some(doc)
                })
                .collect()
        })
        .unwrap_or_default()
}
