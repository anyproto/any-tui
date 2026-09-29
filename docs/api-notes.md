# any-tui — API notes and code layout

How the client maps onto the any REST/SSE API, and how the code is organized.
The user-facing manual is the [README](../README.md); the working notes for
changing the code are in [CLAUDE.md](../CLAUDE.md).

## How it maps onto the API

Worth writing down — little of this is obvious from the swagger.

**Discovering chats.** There is no "list chats" endpoint, and (since the
2026-09 server) no `"chat"` type either. A space has one chat, the general
chat, and its object is a bundle root that is its own type: `any.type` is the
`__type__` marker, `any.collections` holds `miniapp`, `type.layout` is
`{"type": "chat"}`, and it is named `General`. The chat-declaring type ids are
the `owners` of the `chat_messages` dataset in `GET /spaces/{id}/datasets`,
and since a declaring root hosts itself the chat object *is* that id — the
client matches `id` against the owners with `$in`, OR-ed with
`type.layout.type == "chat"` (catches a chat installed after the owners were
read). Filtering on `any.type == <owner>` finds nothing: the root's type slot
holds the marker, and no other object may carry the chat type. Daemons from
before one-type-per-object (2026-09-18) are not supported. Each record's `chat.unreadCount`
/ `chat.unreadMentions` / `chat.unreadReactionsCount` drive the badges, so the
objects subscription doubles as the unread feed. Sidebar order is
`miniapp.pos`.

**Messages** live in the per-object `chat_messages` dataset, read via
`POST /spaces/{id}/query` with `{objectId, dataset, sort, limit}`. Order is
**DAG order** — `sort: ["-_ver.id"]`, the id stamped at creation and never moved
by edits — and history pages with a range filter on it
(`{"_ver.id": {"$lt": <oldest you hold>}}`) rather than `offset`, so a message
landing mid-page can't shift the window. `createdAt` / `modifiedAt` are instants
(`{"$date": "<RFC 3339>"}`); the client also accepts the bare unix-seconds
number older peers/builds still produce.

**Mentions** are markdown links, `[Name](any://m/<spaceId>/<identity>)`. The
server derives `mentions: [identity…]` on each message from those links plus the
replied-to author, and materializes `unreadMention` per message and
`chat.unreadMentions` per chat — plain `@name` text pings nobody. So the composer
turns `@Name` into the link form on send (Tab completes names from
`GET /identities`, scoped by `spaceIds`), and the renderer shows links back as
`@Name` chips using the current display name.

**Read state** per message is the SDK-materialized `unread` / `unreadReactions`
flag on each record; `…/read` covers a message and everything before it, while
a reaction on an older message needs `…/{msgId}/reactions-read` (it's ordered
after its target, so the read cut never reaches it). The "new" divider sits above
the first `unread: true` record — which can be mid-history.

**Sending** stamps `context: {spaceId, objectId, view: "chat"}` — the sender's
view at send time, which agents reading the chat use to resolve "here".

**Search** hits are per *chunk* (long records index as several), so results are
deduped on `(objectId, recordId)`.

**Spaces** joined or left at runtime arrive over `POST /spaces/query/subscribe`
(rows are raw tech-index records, so each frame just triggers a `GET /spaces`).
Before an account is authorized every route but `/health` answers
`401 auth.required`; the client refuses to start with a hint.

**Bao presence** comes off the account **event bus**
(`GET /events/subscribe?scope=account&type=bao.status`, any doc 21) — an
ephemeral, at-most-once channel with no snapshot and no replay, framed
`ready` → `event`* → `closed`. The serving `anyrt` publishes a full-state
`bao.status` beat every 10s and within a second of any change (anybao
ADR-025): `{identity, state, role, winner?, run?: {id, title, startedAt,
cells, cell?}, line?}`. The client keeps the latest beat per publisher, marks
one stale 30s after receipt, and folds them the way any-ui does: a working
beat wins, its label is bao's `line`, else the newest cell's code preview,
else the run title; an idle beat with `role: active` is "bao", all-standby is
"no active bao", none fresh is "offline", none ever is nothing.

**Commands and DMs.** Composer commands are parsed client-side
(`commands.rs`). The only new wire calls are: `PATCH
/spaces/{sp}/objects/{chat}/chat/messages/{id} {text}` for `s/a/b/`; `POST
/spaces/one-to-one {otherIdentity}` → the 1-1 space (idempotent, both peers
derive one id) and `POST /catalog/general-chat/setup {spaceId}` →
`bundles[0].bundle.rootId`, its chat, for `/dm` and `/msg`; `GET
/spaces?status=one_to_one_pending` and `POST /spaces/{id}/one-to-one/accept`
for incoming requests. Settings (`/hl`, `/away`, `/compact`) and the ↑ history
are two documents in the account-scoped local collection `any_tui`
(`PUT /local/collections`, `POST /local/get`, `POST /local/upsert`).

**Attachments.** `GET /spaces/{sp}/files/{fileId}` gives the unsealed name
and size; `GET …/files/{fileId}/content` streams the plaintext (on-demand block
fetch, `409 file.not_available` when unservable). Only `any://f/…` links are
downloadable; web links go to the browser, object links are just labelled.

**Search** fans `POST /spaces/{sp}/search` out over the chosen spaces
concurrently; the "this chat" scope passes `filter: {"id": <chat>}` (matched
against the hit's host object row) so the limit counts that chat's hits. Hits
are enriched with one `$in` query per chat and merged by score (BM25 for fts,
rank-based RRF for hybrid).

**Chat stats, files and links.** The status bar's message count is
`includeTotal` on a 1-row `/query`. The `F` / `L` lists come from one filtered
query for messages with a web link (`$regex`) or an attachment (`$exists`),
paged by `_ver.id`; web links aren't in the link index, which holds only
`any://` references.

**Whispers** are DM messages whose text opens with a link to a message in
another chat (`any://o/<sp>/<chat>/chat_messages/<id>`). The group chat finds
them with the account-wide `GET /v1/backlinks?target=any://o/<sp>/<chat>`
(the per-object route only sees same-space edges), keeps edges from 1-1
spaces, and fetches those DM messages with the usual `$in` query.

**Sync status** in the status bar is `GET /spaces/{id}/sync-status` for the
open chat's space, kept live by `GET /sync-status/subscribe` (sparse `status`
frames, no snapshot) with a 30s re-read.

**Direct peers and devices.** `GET /debug/p2p` (both p2p layers' peers, with
`sources` marking your own devices) splits the per-space direct peers into
other people's and your own, and gives your devices' liveness; `GET /devices`
is the registry (names, OS, versions, the elected bao device) for `/devices`.

**API drift.** `api/openapi.json` pins the daemon's served `GET /v1/openapi.json`
(build recorded in `api/OPENAPI_PIN`); `scripts/api-drift.sh [url]` diffs a running
daemon against it, `--file <swagger.json>` diffs the any repo's generated spec,
and `--update` re-pins.

**Live updates** use SSE (`…/query/subscribe`, `…/objects/query/subscribe`).
Contract, verified against the server source:

- Events are exactly `ready` → `snapshot` → `changes`* → `closed`.
- `changes` data is a JSON **array** of `{versionId, added[], updated[], removed[]}`.
- `removed` entries are `{id, reason}`. Only `reason == "deleted"` means the
  message is gone; `displaced` / `filtered-out` just mean it left the live
  window, so this client keeps those rows.
- `: keepalive` comment frames arrive every 25s and must be skipped.
- **`limit > 0` requires `sort`**, otherwise the request fails with HTTP 500.
- `offset` applies to the initial snapshot only, never the live window — so
  history paging uses plain queries, not the subscription.
- There is no unsubscribe and no subscription id: dropping the connection ends
  it. Switching chats aborts the task; `closed` triggers reconnect with backoff.

## Layout of the code

| file | role |
| --- | --- |
| `api.rs` | REST calls + subscription setup |
| `sse.rs` | SSE frame parser (`ready`/`snapshot`/`changes`/`closed`) |
| `model.rs` | `Space`, `Chat`, `Message` and record parsing |
| `fuzzy.rs` | scored subsequence matcher behind the `Space` picker |
| `commands.rs` | IRC-style composer commands (`/me`, `/dm`, `s///`, …) |
| `prefs.rs` | settings + composer history in the device-local store (`/v1/local`) |
| `files.rs` | attachment download, open (`xdg-open`/`open`) and reveal-in-file-manager |
| `app.rs` | state, event enum, picker, subscription tasks |
| `ui.rs` | rendering, wrapping, scroll geometry, pane layout |
| `main.rs` | CLI, terminal lifecycle, keymap, event loop |

Three kinds of subscription run concurrently:

- **one per space** — chat list and unread counts. This fires only when unread
  or reaction counts change, so it is an *unread* signal, not a *message* one.
- **one per chat**, a window of a single message — keeps the sidebar preview
  live. Needed because a message that doesn't move unread (one you send
  yourself from another device) produces no event on the per-space stream.
- **one for the open chat**, a full window — the message list; aborted when you
  switch chats.
