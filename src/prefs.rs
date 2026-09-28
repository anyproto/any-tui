//! Client settings and composer history, kept in the daemon's **local store**
//! (`/v1/local`, any docs/26-local-store.md): device-only, never synced, and
//! outside every space. One account-scoped collection, two documents.
//!
//! Loading is best-effort — a daemon with the local store disabled still
//! runs the client, just without persistence (`Store::ok` is false).

use crate::api::Api;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const COLL: &str = "any_tui";
const PREFS_ID: &str = "prefs";
const HISTORY_ID: &str = "history";
/// Composer lines kept for ↑/↓ recall.
pub const HISTORY_MAX: usize = 200;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Prefs {
    /// `/hl` words: a message containing one reads like a mention.
    #[serde(default)]
    pub highlights: Vec<String>,
    /// `/away` marker shown after your own name; `None` when back.
    #[serde(default)]
    pub away: Option<String>,
    /// `/compact`: one line per message, IRC-log style.
    #[serde(default)]
    pub compact: bool,
}

pub struct Loaded {
    pub prefs: Prefs,
    pub history: Vec<String>,
    /// False when the local store couldn't be reached; changes then live
    /// only as long as the process.
    pub ok: bool,
}

pub async fn load(api: &Api) -> Loaded {
    let res: anyhow::Result<(Prefs, Vec<String>)> = async {
        api.local_ensure(COLL).await?;
        let prefs = match api.local_get(COLL, PREFS_ID).await? {
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => Prefs::default(),
        };
        let history = api
            .local_get(COLL, HISTORY_ID)
            .await?
            .and_then(|v| v.get("lines").cloned())
            .and_then(|l| serde_json::from_value(l).ok())
            .unwrap_or_default();
        Ok((prefs, history))
    }
    .await;
    match res {
        Ok((prefs, history)) => Loaded { prefs, history, ok: true },
        Err(_) => Loaded {
            prefs: Prefs::default(),
            history: Vec::new(),
            ok: false,
        },
    }
}

pub async fn save_prefs(api: &Api, prefs: &Prefs) -> anyhow::Result<()> {
    let mut doc = serde_json::to_value(prefs)?;
    doc["id"] = Value::from(PREFS_ID);
    api.local_put(COLL, doc).await
}

pub async fn save_history(api: &Api, lines: &[String]) -> anyhow::Result<()> {
    api.local_put(COLL, json!({ "id": HISTORY_ID, "lines": lines })).await
}
