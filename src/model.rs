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

#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub text: String,
    pub creator: String,
    pub created_at: f64,
    pub modified_at: f64,
    pub reply_to: Option<String>,
    /// (emoji, count) pairs, sorted for stable rendering.
    pub reactions: Vec<(String, usize)>,
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
            created_at,
            modified_at,
            reply_to: v
                .get("replyToMessageId")
                .and_then(|t| t.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
            reactions,
        })
    }

    pub fn edited(&self) -> bool {
        self.modified_at > self.created_at + 1.0
    }
}
