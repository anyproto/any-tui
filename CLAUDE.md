# CLAUDE.md — any-tui

A terminal chat reader for [any](https://github.com/anyproto/any) spaces, in Rust.
Talks **only** to the local any REST API (`http://127.0.0.1:7001/v1` by default):
REST for reads/writes, SSE for live updates. No SDK, no direct store access.

`README.md` is the user-facing manual (keymap, layouts, full API notes) — keep it
current when behaviour changes. This file is for working on the code.

## Build / run / test

```sh
cargo build            # or --release
cargo test             # unit tests in fuzzy.rs and model.rs
cargo run -- --no-auto-read      # see flags below
```

Flags: `--api <url>`, `--no-auto-read`, `--layout auto|split|single`.

## Testing against the live daemon

The any daemon runs locally and is **already authorized** — `GET /v1/health`
returns an `account` (the logged-in user is "tolya", `A8qyNqHa…`). This is a real
account with real chats, so testing needs care:

- **Drive the TUI with tmux**, not by hand. Pattern: launch in a detached
  session with a trailing `sleep`, `sleep` to let it connect, `tmux send-keys`,
  `tmux capture-pane -t <sess> -p` (add `-e` for ANSI when checking colours).
  Size it deliberately (`-x 52` reproduces phone-width single-pane mode).
- **Always run tests with `--no-auto-read`.** Opening a chat marks it read, and
  **the API has no un-read endpoint** — you cannot restore unread state. Never
  clear the user's real unread.
- **Only send test messages to the solo `foo` space** (it has no other members),
  and **delete them afterwards** (`DELETE …/chat/messages/{id}`). This
  send-then-delete cycle is pre-approved for `foo`; do not send into shared
  spaces (sync team, dev, bao, Alex). Discover ids at runtime rather than
  hardcoding:
  ```sh
  curl -s http://127.0.0.1:7001/v1/spaces | jq '.spaces[]|{name,id}'
  curl -s -X POST .../spaces/{id}/objects/query -d '{"filter":{"any.types":"chat"},"sort":["nav.pos"],"limit":50}'
  ```
- To verify **live delivery**, send via curl (that is exactly "another device,
  same account") and capture the TUI **without pressing a key** — anything that
  appears came over SSE.

## API contract — the non-obvious parts

Verified against the daemon and the Go source in `../any` (server code under
`internal/server/`). Most of this is not in the swagger.

- **No "list chats" endpoint.** Chats are objects with `any.types: ["chat"]`,
  found per space via `POST /spaces/{id}/objects/query`. `chat.unreadCount` /
  `chat.unreadReactionsCount` on each object drive the badges.
- Spaces usually hold **several chats all named `general`** plus one unnamed,
  nav-less chat (shown as "space chat"). The last-message preview is what
  distinguishes them — it is load-bearing, not decoration.
- **Messages** live in the per-object `chat_messages` dataset
  (`POST /spaces/{id}/query` with `{objectId, dataset, sort, limit, offset}`).
  `creator` is an identity address; resolve names via `GET /identities`. The
  `query` endpoint also takes a `filter` — e.g. `{"id":{"$in":[…]}}` fetches
  specific messages by id (used to enrich search hits).
- **Agent messages** carry an optional `agent` object `{name, debugLink?, done}`
  — its *presence* is the only marker, and `creator` stays the **human account**
  (agents sign as the signer, not a distinct identity; nothing in `/identities`
  or members flags an agent). So render by `agent`, never by creator, or agent
  replies show as the account owner and fold into their message group. `done ==
  false` means the run is still streaming (show a "working…" hint on the trailing
  message only). Old run-start pings post a bare `"…"` text — hide those.
- **Search** is `POST /spaces/{id}/search` — **per-space only**, there is no
  global `/v1/search` (404). Body: `{query, scopes[], limit (≤100), mode,
  require[], exclude[]}`. `scopes:["chat"]` restricts to chat messages (the
  only scope we use); there is **no** objectId/offset filter, so single-chat
  scoping and paging are client-side. `mode` is `hybrid|fts|vector` (semantic
  is `vector`, not `"semantic"`). Hits are `{scope, objectId (the chat),
  recordId (the message id), data (text), score}` — **no creator/timestamp**,
  so enrich via the `$in` query above. The index is **forward-only** (content
  written before indexing isn't found) and `vectorStatus` says whether the
  semantic leg ran (`used|unavailable|disabled|skipped`).
- **SSE** (`…/query/subscribe`, `…/objects/query/subscribe`): events are exactly
  `ready` → `snapshot` → `changes`* → `closed`. `changes` data is a JSON
  **array** of `{versionId, added[], updated[], removed[]}`. `removed` entries
  are `{id, reason}`; only `reason == "deleted"` means gone (`displaced` /
  `filtered-out` just left the window — keep those rows). `: keepalive` comments
  arrive every 25s and must be skipped.
- **`limit > 0` requires `sort`**, else the request fails with **HTTP 500**.
- **`offset` applies to the snapshot only**, never the live window — history
  paging uses plain queries, not the subscription.
- **No unsubscribe and no subscription id** — drop the connection to end it.
- **Mentions are not implemented server-side** (`unreadMention` never sets); see
  memory `mentions-deferred`. Deferred until upstream adds them — do not fake it
  with plain `@name` text.
- **Sending attachments is unsupported** (`ChatSendRequest` is text/reply/agent/
  attachments and the CRDT handler rejects unknown keys). Incoming attachments
  are shown as `📎 N images`; that's the whole feature for now.

## Architecture

`main.rs` owns the event loop: a channel of `Ev`, drained per frame, one redraw.
Terminal input runs on a blocking thread; a 1s tick expires toasts and drives the
auto-read dwell. All network work happens in spawned tasks that send `Ev`s back.

**Three kinds of subscription run at once — know which signal is which:**

1. **Per space** (`spawn_chats_sub`) → chat list + unread counts. Fires **only
   when unread/reaction counters change**. It is an *unread* signal, not a
   *message* signal. A message you send yourself keeps your unread at 0, so this
   stream emits nothing for it.
2. **Per chat, window of 1** (`spawn_preview_sub`) → keeps the sidebar preview
   live independent of unread. Exists precisely because (1) misses self-sent /
   unread-neutral messages. Spawned once per chat, aborted on removal.
3. **Active chat, full window** (`spawn_messages_sub`, `WINDOW=150`) → the
   message list; aborted when you switch chats.

Code map: `api.rs` (REST + subscribe setup), `sse.rs` (frame parser),
`model.rs` (`Space`/`Chat`/`Message` + record parsing, has tests), `fuzzy.rs`
(scored subsequence matcher for the picker, has tests), `app.rs` (state, `Ev`,
picker, subscription tasks), `ui.rs` (rendering, wrapping, scroll geometry,
layout), `main.rs` (CLI, terminal, keymap, loop).

## Invariants — don't regress these

- **Auto-read is one-way and must stay conservative.** It fires only when the
  message cursor is on the newest message, after a **1.5s dwell** (`DWELL`), and
  in single-pane mode only when the message pane is actually visible. Because the
  cursor previews chats as it moves and there is no un-read endpoint, the bias is
  always toward *not* marking. `--no-auto-read` must fully disable it.
- **Selection is shown with brighter text, not a background tint.** A `Line`
  background only paints cells with text, so it looks ragged on truncated rows.
  Applies to sidebar, picker, and the message cursor (`▌` bar).
- **Picker items are keyed by `object_id`, not list index.** The chat list
  re-sorts by recency on every new message; a stale index opens the wrong chat.
- Chats sort by space order, then most-recent-activity, then nav pos.
- The message cursor drives the viewport (scroll follows it at render time);
  `r` replies to the message under the cursor, not "the newest".
- Layout auto-collapses to one pane below `NARROW_COLS = 80`; `z` toggles and
  pins.

## Conventions

- Commit only when asked. Messages end with the `Co-Authored-By` trailer.
- Prefer verifying wire behaviour against the live daemon or the `../any` source
  over inferring from swagger — the swagger omits and occasionally misstates
  (e.g. it documents `removed` as bare id strings; the wire sends objects).
- Keep unit tests for pure logic (parsing, fuzzy scoring); drive I/O-bound
  behaviour through tmux.
