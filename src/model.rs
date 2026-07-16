use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct Space {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Identity {
    pub identity: String,
    #[serde(default)]
    pub name: String,
}

/// A chat object inside a space. `unread` drives the sidebar badges and the
/// status bar, and is kept live by the per-space objects subscription.
#[derive(Debug, Clone)]
pub struct Chat {
    pub space_id: String,
    pub space_name: String,
    pub object_id: String,
    pub name: String,
    pub unread: u64,
    pub unread_reactions: u64,
    pub pos: String,
    /// Preview of the newest message. Spaces routinely hold several chats all
    /// named "general", so the preview is what actually tells them apart.
    pub last_text: Option<String>,
    pub last_creator: String,
    /// Agent name when the newest message was agent-authored — the preview
    /// shows this instead of the human account that signed it.
    pub last_agent: Option<String>,
    pub last_at: f64,
}

impl Chat {
    /// Chat objects carry `any.types: ["chat", ...]`; the `chat` sub-document
    /// holds the unread counters. Returns None for records that aren't chats.
    pub fn from_record(v: &Value, space_id: &str, space_name: &str) -> Option<Chat> {
        let id = v.get("id")?.as_str()?.to_string();
        let any = v.get("any");
        let is_chat = any
            .and_then(|a| a.get("types"))
            .and_then(|t| t.as_array())
            .map(|ts| ts.iter().any(|t| t.as_str() == Some("chat")))
            .unwrap_or(false);
        if !is_chat {
            return None;
        }
        let name = any
            .and_then(|a| a.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let pos = v
            .get("nav")
            .and_then(|n| n.get("pos"))
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        let chat = v.get("chat");
        Some(Chat {
            space_id: space_id.to_string(),
            space_name: space_name.to_string(),
            object_id: id,
            name,
            unread: chat
                .and_then(|c| c.get("unreadCount"))
                .and_then(|c| c.as_u64())
                .unwrap_or(0),
            unread_reactions: chat
                .and_then(|c| c.get("unreadReactionsCount"))
                .and_then(|c| c.as_u64())
                .unwrap_or(0),
            pos,
            last_text: None,
            last_creator: String::new(),
            last_agent: None,
            last_at: 0.0,
        })
    }

    /// Label for the sidebar. The unnamed, nav-less chat object is the
    /// space-level chat; named ones are ordinary chats in the nav tree.
    pub fn label(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else {
            "space chat"
        }
    }

    pub fn qualified(&self) -> String {
        format!("{}/{}", self.space_name, self.label())
    }
}

/// Marks a message written by an AI agent. Agent messages are posted under the
/// human account (so `creator` is the account, not a distinct identity) — the
/// presence of this field is the only thing that tells them apart. `done` is
/// false while the agent is still streaming its reply.
#[derive(Debug, Clone)]
pub struct Agent {
    pub name: String,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub text: String,
    pub creator: String,
    /// Set when an agent authored this message; see [`Agent`].
    pub agent: Option<Agent>,
    pub created_at: f64,
    pub modified_at: f64,
    pub reply_to: Option<String>,
    /// (emoji, count) pairs, sorted for stable rendering.
    pub reactions: Vec<(String, usize)>,
    /// Attachment kinds ("image", "file", …), one per attached file. Sending
    /// attachments isn't supported yet; this is only to show they exist.
    pub attachments: Vec<String>,
}

impl Message {
    pub fn from_record(v: &Value) -> Option<Message> {
        let id = v.get("id")?.as_str()?.to_string();
        let created_at = v.get("createdAt").and_then(|c| c.as_f64()).unwrap_or(0.0);
        let modified_at = v
            .get("modifiedAt")
            .and_then(|c| c.as_f64())
            .unwrap_or(created_at);
        let mut reactions: Vec<(String, usize)> = v
            .get("reactions")
            .and_then(|r| r.as_object())
            .map(|obj| {
                obj.iter()
                    .map(|(emoji, who)| {
                        let n = who.as_object().map(|o| o.len()).unwrap_or(0);
                        (emoji.clone(), n)
                    })
                    .filter(|(_, n)| *n > 0)
                    .collect()
            })
            .unwrap_or_default();
        reactions.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        // {"f0": {"type": "image", "link": "any://<space>/files/<id>"}, …}
        let attachments: Vec<String> = v
            .get("attachments")
            .and_then(|a| a.as_object())
            .map(|obj| {
                obj.values()
                    .map(|a| {
                        a.get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("file")
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(Message {
            id,
            text: v
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            creator: v
                .get("creator")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            // {"agent": {"name": "bao", "done": true}} on agent-authored messages.
            agent: v.get("agent").and_then(|a| a.as_object()).map(|a| Agent {
                name: a
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("agent")
                    .to_string(),
                done: a.get("done").and_then(|d| d.as_bool()).unwrap_or(true),
            }),
            created_at,
            modified_at,
            reply_to: v
                .get("replyToMessageId")
                .and_then(|t| t.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
            reactions,
            attachments,
        })
    }

    pub fn edited(&self) -> bool {
        self.modified_at > self.created_at + 1.0
    }

    /// Old agent run-start pings post a bare "…" placeholder; it carries no
    /// content and shouldn't show as a message.
    pub fn is_agent_presence_marker(&self) -> bool {
        self.agent.is_some() && self.text.trim() == "…"
    }

    /// e.g. "3 images", "1 image, 2 files". Empty when nothing is attached.
    pub fn attachment_summary(&self) -> String {
        if self.attachments.is_empty() {
            return String::new();
        }
        let mut kinds: Vec<(String, usize)> = Vec::new();
        for a in &self.attachments {
            match kinds.iter_mut().find(|(k, _)| k == a) {
                Some((_, n)) => *n += 1,
                None => kinds.push((a.clone(), 1)),
            }
        }
        kinds
            .iter()
            .map(|(k, n)| {
                if *n == 1 {
                    format!("1 {k}")
                } else {
                    format!("{n} {k}s")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// What to show in a one-line preview. An image-only message has no text,
    /// so fall back to describing the attachments rather than showing nothing.
    pub fn preview_text(&self) -> String {
        if !self.text.trim().is_empty() {
            return self.text.clone();
        }
        match self.attachment_summary().as_str() {
            "" => String::new(),
            s => format!("📎 {s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape the server actually sends, taken from a live response.
    fn record_with_attachments(text: &str, kinds: &[&str]) -> Value {
        let mut atts = serde_json::Map::new();
        for (i, k) in kinds.iter().enumerate() {
            atts.insert(
                format!("f{i}"),
                json!({"type": k, "link": "any://space.x/files/abc"}),
            );
        }
        json!({
            "id": "m1",
            "text": text,
            "creator": "A8qy",
            "createdAt": 1784141327.0,
            "attachments": Value::Object(atts),
        })
    }

    #[test]
    fn parses_attachment_kinds() {
        let m = Message::from_record(&record_with_attachments("scr", &["image"])).unwrap();
        assert_eq!(m.attachments, vec!["image"]);
        assert_eq!(m.attachment_summary(), "1 image");
    }

    #[test]
    fn groups_and_pluralises() {
        let m =
            Message::from_record(&record_with_attachments("x", &["image", "image", "image"]))
                .unwrap();
        assert_eq!(m.attachment_summary(), "3 images");
    }

    #[test]
    fn summarises_mixed_kinds() {
        let m = Message::from_record(&record_with_attachments("x", &["image", "file"])).unwrap();
        let s = m.attachment_summary();
        assert!(s.contains("1 image") && s.contains("1 file"), "{s}");
    }

    #[test]
    fn no_attachments_yields_empty_summary() {
        let m = Message::from_record(&json!({"id":"m","text":"hi","createdAt":1.0})).unwrap();
        assert_eq!(m.attachment_summary(), "");
        assert_eq!(m.preview_text(), "hi");
    }

    #[test]
    fn image_only_message_previews_as_attachment() {
        // Otherwise an image-only message shows a blank row in the sidebar.
        let m = Message::from_record(&record_with_attachments("", &["image"])).unwrap();
        assert_eq!(m.preview_text(), "📎 1 image");
    }
}
