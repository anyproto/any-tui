# CLAUDE.md — any-tui

A terminal chat reader for [any](https://github.com/anyproto/any) spaces, in Rust.
Talks **only** to the local any REST API (`http://127.0.0.1:7001/v1` by default):
REST for reads/writes, SSE for live updates. No SDK, no direct store access.

`README.md` is the brief user-facing manual (quick start incl. curl auth, the
anybao agent setup, keymap, flags); `docs/api-notes.md` holds the full API
mapping and the code layout. Keep both current when behaviour changes. This
file is for working on the code.

## Build / run / test

```sh
cargo build            # or --release
cargo test             # unit tests: model.rs, commands.rs, files.rs, fuzzy.rs, app.rs
cargo run -- --no-auto-read      # see flags below
nix run . -- --no-auto-read      # the flake's app; `nix build` → ./result/bin/any-tui
```

The flake builds from git-tracked files only — `git add -N` a new source file
before `nix build`, or it compiles without it. The tree is not rustfmt-clean;
don't reformat wholesale in a feature change.

Flags: `--api <url>`, `--no-auto-read`, `--layout auto|split|single`.

## Testing against the live daemon

The any daemon runs locally and must be **authorized** — `GET /v1/health`
returns an `account`; if it is `""`, authorize with curl (README § Quick
start — `POST /v1/auth` with `{}` generates, with a `mnemonic` restores). **Which account `:7001` is has changed over time** —
since 2026-09-21 it is the throwaway `any-prod-test-2` account (`A9fBcRQu…`,
named "Ann", data dir `~/any/any-prod-test-2`, mnemonic in its `ACCOUNT.txt`
and in `~/.any-accounts/`); before that it was the `repo-prod2` agent-repo
account. **Its DM partner is `:7002`** — the throwaway `any-prod-test-3`
("Bob", `A9Q7AUCZ…`, data dir `~/any/any-prod-test-3`, mnemonic likewise),
created 2026-09-29 for DM/whisper tests. Both run the same `bin/any`
(`nix develop -c ./bin/any run --data-dir <dir> --addr 127.0.0.1:<port>` from
`../any`). They share `tui-test` (Ann's) and `bob-test` (Bob's) and have a
1-1; drive one TUI per account in two tmux sessions. Check `/health` and
`/spaces` before assuming. On the throwaway account send/delete freely; on anything
else it is a real account with real chats, so testing needs care:

- **Drive the TUI with tmux**, not by hand. Pattern: launch in a detached
  session with a trailing `sleep`, `sleep` to let it connect, `tmux send-keys`,
  `tmux capture-pane -t <sess> -p` (add `-e` for ANSI when checking colours).
  Size it deliberately (`-x 52` reproduces phone-width single-pane mode).
- **Always run tests with `--no-auto-read`.** Opening a chat marks it read, and
  **the API has no un-read endpoint** — you cannot restore unread state. Never
  clear the user's real unread.
- **Only send test messages on the throwaway accounts** (Ann `:7001`, Bob
  `:7002` — `tui-test`, `bob-test`, their DM), and **delete them afterwards**
  (`DELETE …/chat/messages/{id}`). Never send, `/dm`, rename or accept on a
  real account (e.g. `:7009`): read-only there, with `--no-auto-read`; and
  note `/compact`, `/icons`, `/hl` write that account's local prefs — put
  them back. Discover ids at runtime rather than hardcoding:
  ```sh
  curl -s http://127.0.0.1:7001/v1/spaces | jq '.spaces[]|{name,id}'
  curl -s .../spaces/{id}/datasets | jq '.datasets[]|select(.name=="chat_messages")|.owners'
  curl -s -X POST .../spaces/{id}/objects/query -d '{"filter":{"type.layout.type":"chat"},"limit":50}'
  ```
- To verify **live delivery**, send via curl (that is exactly "another device,
  same account") and capture the TUI **without pressing a key** — anything that
  appears came over SSE.

## API contract — the non-obvious parts

Verified against the daemon and the Go source in `../any` (server code under
`internal/server/`, contract docs in `docs/03-api.md`, `docs/04-events.md`,
`docs/16-chat.md`, `docs/19-links.md`). Most of this is not in the swagger.

**Drift check first.** `api/openapi.json` pins the served `GET /v1/openapi.json`
of the build this client was last adjusted to (`api/OPENAPI_PIN` names it).
Before touching the API layer run `scripts/api-drift.sh [url]` against the
daemon you're targeting (or `--file ../any/internal/server/docs/swagger.json`
after `make swagger` in `../any`); when done adjusting, `--update` re-pins.
The served form is authoritative — it stamps `additionalProperties: false` on
strictly-bound request schemas, which the generated file cannot.

- **No "list chats" endpoint, and no `"chat"` type any more.** Since the
  2026-09 nightlies (SYN-216/233) a space has **one chat**, the general chat:
  a bundle root that is **its own type**. Since one-type-per-object (any
  81e7456, 2026-09-18, in every build from b5be51c on) an object has a scalar
  `any.type` and an `any.collections` array, and the chat's row is
  `any.type: "__type__"`, `any.collections: ["miniapp"]`, `type.xkey:
  "general_chat"`, `type.layout: {"type": "chat"}`, `any.name: "General"`,
  `miniapp.bundle: "system:general-chat/v1"` (sidebar order is
  `miniapp.pos`, absent until set). The recipe: the chat-declaring type ids
  are the `owners` of `chat_messages` in `GET /spaces/{id}/datasets`, and
  because a declaring root **hosts itself** the chat object IS that id — its
  own id appears nowhere on its row but `id`. So match `id` with `$in` on the
  owners (`Api::chat_type_ids`); `any.type == <owner>` matches NOTHING (the
  type slot holds the marker, and no other object may carry the chat type:
  `type.reserved_carrier`). We OR that with `type.layout.type == "chat"`
  (catches a chat installed after the owners were read). Builds before
  b5be51c (the `any.types` array, the literal `"chat"` type, `nav.pos`) are
  deliberately not supported — every account is new. Verified 2026-09-21 on
  0a1eaf4 and b5be51c. Filtering on an absent path is not an error — a wrong
  leg lists ZERO chats silently. It is
  installed by `POST /catalog/general-chat/setup` (a write; we call it only
  for a DM we open or accept, never on an ordinary space); a space without
  it has no chat. `chat.unreadCount` /
  `chat.unreadMentions` / `chat.unreadReactionsCount` on the row drive the
  badges and are absent until first materialized (treat absent as 0).
- **Messages** live in the per-object `chat_messages` dataset
  (`POST /spaces/{id}/query` with `{objectId, dataset, sort, limit}`).
  **Order and page by `_ver.id`** (`sort: ["-_ver.id"]`, older page =
  `filter: {"_ver.id": {"$lt": <oldest held>}}`) — DAG order, stamped at
  creation, never bumped by edits; `offset` still exists but shifts under live
  arrivals. **Timestamps are instants** `{"$date": "<RFC 3339>"}` since
  2026-08; `model::parse_instant` also takes the bare unix-seconds number older
  peers still write — keep both. `creator` is an identity address; resolve
  names via `GET /identities`. The `query` endpoint also takes a `filter` —
  e.g. `{"id":{"$in":[…]}}` fetches specific messages by id (used to enrich
  search hits). Records carry SDK-materialized `unread` / `unreadMention` /
  `unreadReactions` flags (absent once read).
- **Request bodies are strict**: every schema we post (`SpaceQueryRequest`,
  `SearchRequest`, `ChatSendRequest`, …) is `additionalProperties: false` on
  the served spec, so an unknown key is a 400 `request.unknown_field`, not
  ignored. Check `api/openapi.json` before adding a field.
- **Mentions are implemented server-side** (since 2026-08): a mention is the
  link `[Name](any://m/<spaceId>/<identity>)`; the server derives `mentions[]`
  from links + replied-to author and badges `chat.unreadMentions`. Plain
  `@name` text pings nobody, so `send_input` rewrites `@Name` via
  `model::link_mentions` and the UI renders links back as `@Name` chips
  (`model::render_mentions`). Roster = `GET /identities` (`spaceIds`).
- **IRC commands are client-side** (`commands.rs`, pure + tested). `/me` is a
  convention, not a wire field: sent as the literal text `/me waves` (other
  clients show it verbatim), rendered as `* Name waves`
  (`model::action_body`). Unknown `/word`s are refused so a typo never goes
  out; `//` escapes. Faces are sent markdown-escaped (any-ui renders
  markdown: a bare `\_` eats the shrug's arm) and every display path runs
  `model::md_unescape`. `s/a/b/` edits via `PATCH …/chat/messages/{id}`
  `{text}`. **Test DMs only between throwaway accounts** — `/dm` on a real
  account opens a real 1-1 on the prod network.
- **DMs are 1-1 spaces** (`spaceType: "any.onetoone"`, named after the peer by
  the server, already carrying a general chat). `POST /spaces/one-to-one
  {otherIdentity}` is idempotent and returns the space (both peers derive the
  same id); then `general-chat/setup` (idempotent, derived root) gives the
  chat id, and `Ev::OpenChat` opens it once the space-list subscription
  delivers it. Incoming requests are `GET /spaces?status=one_to_one_pending`
  (re-read on every space-list change), approved by `POST
  /spaces/{id}/one-to-one/accept`.
- **DM naming**: a 1-1 you just opened has **no `name`** until the peer's
  profile decrypts (after they accept), so the list labels it `@<peer>` from
  `GET /spaces/{id}/members` (the non-me member). Chats take their space
  label from the **current** listing (`App::space_label` in `upsert_chat` and
  `set_spaces`) — the chat-subscription task holds a stale `Space` clone.
  Identities are re-read on every space-list change, since names arrive late.
  Pending requests: `GET /spaces?status=one_to_one_pending` (DMs) and
  `status=invite_pending` (someone ran `acl/add` on you; accept with `POST
  /spaces/{id}/invite/accept`) — polled every 30s too, as a pending row does
  not always move the space-list stream.
- **Sync status** (status bar): `GET /spaces/{id}/sync-status` → `{state:
  unknown|offline|syncing|synced|error, synced, total, networkPeers,
  localPeers, globalPeers, p2p: unknown|notpossible|notconnected|connected|
  restricted}` — the peer counts are live connections syncing THAT space
  (nodes, LAN/mDNS, internet-wide direct over iroh; any docs/30-global-p2p.md).
  Live: `GET /sync-status/subscribe` (account-wide SSE, `ready` → sparse
  `status` frames with the same body, `lagged`), **no snapshot** — read the
  GET for every space at startup / for new spaces, and again for all on
  (re)connect and on `lagged`; the stream carries every space's flips, and a
  30s re-read of the open chat's space backs it up. The status bar shows the
  open chat's space; the chat list badges each space header `lan` / `p2p`
  (`✗` offline) — verified live 2026-09-29 by stopping and restarting Bob. Both test accounts run on this box, so Ann and Bob see each other over
  LAN and iroh at once. `GET /debug/p2p` has the device-level picture
  (relay session, discovered peers) — not used by the client.
- **Own devices vs other people's** (`GET /debug/p2p`): the sync-status peer
  counts include your own other devices, which find each other through the
  account record and sync EVERY space — so on a multi-device account every
  space reads `globalPeers ≥ 1`. `/debug/p2p` lists peers in both layers
  (`peers` = LAN, `global.peers` = iroh) with `connected`, `lastSeen`,
  `tier`, `spaceIds` and `sources` (`account` = your own device);
  `model::direct_peers` folds it per space (lan / p2p = others, own), and
  both the badges and the status bar use that, falling back to the plain
  counts when the route is absent. No stream: re-read ≤5s after a sync flip
  and every 30s.
- **Devices** (`GET /devices`, any docs/23-devices.md): the account's
  registry — `{devices: [{peerId, name, os, version, apps, activeClaims}],
  self, active: {<app>: <peerId>}}` (the server computes the election).
  **It holds no liveness by design** (a heartbeat would churn the CRDT);
  bao's election is just one app's claim on it. Online / last seen comes
  from `/debug/p2p`, whose `peerId` is the registry's row id. `/devices`
  shows the join.
- **Identities parse per row** (`Api::identities`): the wire sends
  `"spaceIds": null` for some identities, and one such row in a whole-list
  parse once wiped every name (the chat showed only id prefixes). Nullable
  fields go through `null_as_default`.
- **Emoji vs terminal width tables** (`/icons safe|off|full`,
  `model::terminal_safe`, `ui::sanitize`): tmux, mosh and the outer
  terminal each have their own wcwidth, and they disagree on emoji newer
  than Unicode 9 and on ZWJ / VS16 / skin-tone sequences — the cursor
  drifts and ratatui's diff redraw leaves stale cells (seen over
  mosh → tmux on macOS, 2026-09-29). **tmux `capture-pane` shows tmux's
  grid, not the outer terminal, so it never shows this.** The fix is a pass
  over the finished frame buffer: composed graphemes → their base, newer
  emoji → `◌` (`off`: common ones → 2-column text, UI markers kept),
  never wider than ratatui allotted. Icons with newer emoji are dropped in
  `safe`. `Ctrl-L` sets `App::clear_screen` → `terminal.clear()`.
- **Member icons** are the identity's `iconCid` (`GET /identities`): a bare
  emoji (`"🧉"`), an `icon:v2:{"assignment":{"kind":"emoji","grapheme":…}}`
  or `{"kind":"pack","ref":{"packId","glyphId"}}` (lucide / iconoir), or a
  picture ref. `model::icon_glyph` renders the first two (pack glyphs through
  a small Unicode table, unmapped ones → none) and deliberately never
  pictures. Spaces carry the same forms in their own `iconCid`; a DM uses
  the peer's profile icon (`App::space_icon`), and an `icon:v2` `color`
  (any-ui's palette names) tints the glyph (`ui::icon_palette`). Pack glyph
  ids are matched after dropping variant words (`Solid`, `Off`, `Circle`),
  then by arrow direction. `/compact` also collapses the chat list to one
  row per chat (`selected_line_index` knows both layouts).
  `PUT /account/metadata` is **full-replace** — send the name with
  the icon or the name is wiped. `/nick` (`Api::rename`) therefore reads
  `GET /account` → `metadata` first, re-sends description and icon, and
  refuses to write when that read fails; it applies the new name locally
  (`Ev::Renamed`) because the directory can lag the PUT for a moment.
- **Whispers** are DM messages about a message elsewhere: the text opens with
  `[↪ Author: quote](any://o/<sp>/<chat>/chat_messages/<msgId>)` then the
  private body (`model::whisper_text` / `parse_whisper`). Readable in any
  client, private because it lives in the 1-1. To show them under the target
  message, **`GET /v1/backlinks?target=any://o/<sp>/<chat>`** (account-wide,
  `parts` = edges to the chat's messages from every space) — the per-object
  `…/objects/{chat}/backlinks` route does NOT include edges from other
  spaces. Keep edges whose source space is a 1-1 and whose text parses as a
  whisper for this chat; refreshed with the chat stats and when a DM
  preview carries a whisper for the open chat. Verified 2026-09-29.
- **Settings live in the local store** (`prefs.rs`; any
  `docs/26-local-store.md`): account-scoped collection `any_tui`, docs
  `prefs` and `history`. Device-only, never synced, not subscribe-able.
  `PUT /local/collections` must run before any write
  (`local.collection_not_found`); a missing doc is `404 local.doc_not_found`.
  Load is best-effort — a daemon with `local.enabled: false` still runs the
  client, just without persistence.
- **Send** stamps `context: {spaceId, objectId, view}` (create-only; agents
  resolve "here" from it). A pre-2026-08 daemon would 400 on the key.
- **`…/{msgId}/reactions-read`** clears unread reactions on one message —
  `…/read` cuts at the message's own `_ver.id` and never covers a later
  reaction. `maybe_mark_read` calls it for loaded messages flagged
  `unreadReactions`, once each.
- **Space list is live**: `POST /spaces/query/subscribe` (`{"limit":0}`) fires
  on join/leave; rows are raw tech-index records, so we just re-`GET /spaces`
  and reconcile (`App::set_spaces`).
- **Auth guard**: until an account is authorized every route except
  `/health`, `/openapi.json`, `/auth` answers `401 auth.required`. `/health`
  has `account: ""` in that state — we bail with a hint at startup.
- **The general chat is permanent and unique**: a derived root (`DELETE` is
  `409 object.derived_undeletable`), and the `chat` module is reserved to the
  server, so `POST /objects` never makes a chat. A space set up under the
  older client recipe may keep its old root too, in which case
  `chat_messages.owners` lists two ids — both are real chats.
- **Agent messages** carry an optional `agent` object `{name, debugLink?, done}`
  — its *presence* is the only marker, and `creator` stays the **human account**
  (agents sign as the signer, not a distinct identity; nothing in `/identities`
  or members flags an agent). So render by `agent`, never by creator, or agent
  replies show as the account owner and fold into their message group. `done ==
  false` means the run is still streaming (show a "working…" hint on the trailing
  message only). Old run-start pings post a bare `"…"` text — hide those.
- **Search** is `POST /spaces/{id}/search` — **per-space only**, there is no
  global `/v1/search` (404), so "all spaces" fans out one request per space
  (concurrently; a failing space only loses its hits). Body: `{query,
  scopes[], limit (≤100), mode, require[], exclude[], maxData, filter,
  passages}`. `scopes:["chat"]` restricts to chat messages (the only scope we
  use). **`filter` matches the hit's host object row** (`/objects/query`
  grammar), so `{"id": <chat>}` narrows to one chat server-side and `limit`
  then counts that chat's hits — the "this chat" scope uses it. No offset:
  paging is not possible. Scores: fts is BM25; **hybrid is RRF,
  `1/(60+rank)`, so every space's #1 hit scores the same** — merging spaces
  by score interleaves them by rank (ties → newer). `mode` is `hybrid|fts|vector`
  (semantic is `vector`, not `"semantic"`). Hits are `{scope, dataset, objectId
  (the chat), recordId (the message id), chunk, data (windowed text),
  dataOffset, dataTotal, score}` — **no creator/timestamp**, so enrich via the
  `$in` query above. **Long records index as several chunks, each a separate
  hit** — dedupe on `(objectId, recordId)` (done in `Api::search`). The index
  is **forward-only** (content written before indexing isn't found) and
  `vectorStatus` says whether the semantic leg ran
  (`used|unavailable|disabled|skipped`).
- **Chat stats and the files/links lists** (`F` / `L`): the message count is
  `includeTotal` on a 1-row query. Files and links come from ONE filtered
  query — `{"$or": [{"text": {"$regex": "https?://"}}, {"attachments":
  {"$exists": true}}]}`, paged by `_ver.id` (capped at 5000 messages) — then
  attachments and `model::extract_urls` split them. The link index
  (`…/objects/{chat}/links`) is **not** usable for this: it records only
  `any://` references (files, objects, mentions), never web URLs. Refreshed on
  chat open and, throttled to 3s, after message events.
- **SSE** (`…/query/subscribe`, `…/objects/query/subscribe`): events are exactly
  `ready` → `snapshot` → `changes`* → `closed`. `changes` data is a JSON
  **array** of `{versionId, added[], updated[], removed[]}`. `removed` entries
  are `{id, reason}`; only `reason == "deleted"` means gone (`displaced` /
  `filtered-out` just left the window — keep those rows). `: keepalive` comments
  arrive every 25s and must be skipped.
- **`limit > 0` requires `sort`**, else the request fails with **HTTP 500**.
- **`offset` applies to the snapshot only**, never the live window — history
  paging uses plain queries (by `_ver.id` range), not the subscription.
- **No unsubscribe and no subscription id** — drop the connection to end it.
- **`closed` reasons** are `server_shutdown | sdk_closed | overflow | drifted`;
  all mean "open a fresh POST" (we reconnect with backoff). `mailboxCapacity`
  (default 256) and `driftBudgetPercent` (default 30) on the request body tune
  the last two.
- **Bao presence is an event-bus subscription, not a dataset.** `GET
  /events/subscribe?scope=account&type=bao.status` (any `docs/21-events.md`)
  is SSE with `ready` → `event`* → `closed`, **no snapshot and no replay**:
  a subscriber sees only beats published after it connects, so the bar is
  blank until the first one. The serving `anyrt` beats every 10s and within
  1s of any change (anybao ADR-025): `data: {identity, state: boot|idle|
  working|shutdown, role: active|standby, winner?, run?: {id, title,
  startedAt, cells, cell?}, line?, lineAt?}`. Fold per `identity`, measure
  staleness on our receipt clock (offline after 30s), and never read
  liveness as "online" — a standby device beats too; only `role` says who
  answers. `model::derive_bao_presence` is that fold; the bar shows
  `line`, else `run.cell` (the newest tool call's ≤48-char code preview),
  else `run.title`. Verified on the wire 2026-09-21.
- **Attachments** are `{"a01": {type, link}, …}` on the record. `link` is
  `any://f/<spaceId>/<fileId>` for a files-v2 file (older rows:
  `any://<spaceId>/files/<fileId>`), `any://o/…` for an object, or a plain
  `https://` URL — `model::Attachment::target` sorts them. Name/size come
  from `GET /spaces/{sp}/files/{fileId}` (fetched once per file as messages
  load); bytes from `…/files/{fileId}/content` (streams, `Range`, fetches
  missing blocks on demand, `409 file.not_available` when no peer has it
  and it isn't backed up). `files.rs` does the rest: `o` downloads into
  `~/.cache/any-tui/files/<fileId>/` (macOS `~/Library/Caches`) and runs
  `xdg-open` / `open`; `s` saves into the XDG download dir without
  overwriting, then reveals the files in one window (`open -R`, or the
  freedesktop `FileManager1.ShowItems` D-Bus call, falling back to
  `xdg-open` on the folder). reqwest is built **without TLS** — web-link
  attachments are handed to the browser, never fetched. **Sending**
  attachments is still unsupported.

## Architecture

`main.rs` owns the event loop: a channel of `Ev`, drained per frame, one redraw.
Terminal input runs on a blocking thread; a 1s tick expires toasts and drives the
auto-read dwell, the throttled chat-stats and `/debug/p2p` re-reads, and the
30s pending-request / sync-status backstops. All network work happens in
spawned tasks that send `Ev`s back. Each frame ends with `ui::sanitize`, the
emoji pass over the finished buffer.

**Five subscriptions run at once — know which signal is which** (besides the
three below: `spawn_bao_sub`, the account event bus filtered to `bao.status`,
feeding only the status bar's bao segment, reconnecting silently since an
account without a serve never beats; and `spawn_sync_sub`, the account-wide
`/sync-status/subscribe` flips behind the sync segment and the `lan` / `p2p`
badges):

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
(scored subsequence matcher for the picker, has tests), `commands.rs` (composer
command parser, has tests), `prefs.rs` (local-store settings + history),
`files.rs` (attachment download / open / reveal, has tests),
`app.rs` (state, `Ev`,
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
- Chats sort DMs first, then spaces, each group alphabetical by label
  (case-insensitive), then most-recent-activity, then sidebar pos
  (`miniapp.pos`).
- The message cursor drives the viewport (scroll follows it at render time);
  `r` replies to the message under the cursor, not "the newest".
- Layout auto-collapses to one pane below `NARROW_COLS = 80`. `z` is a separate
  axis — `App::sidebar_hidden`, "show/hide the chat list", honoured at any
  width; hiding forces `Focus::Messages` (nothing else is left to focus) and
  `back_to_list` (Esc) un-hides, so Esc always means "back to the chats".

## Conventions

- Commit only when asked. Messages end with the `Co-Authored-By` trailer.
- Prefer verifying wire behaviour against the live daemon or the `../any` source
  over inferring from swagger — the swagger omits and occasionally misstates
  (e.g. it documents `removed` as bare id strings; the wire sends objects).
- Keep unit tests for pure logic (parsing, fuzzy scoring); drive I/O-bound
  behaviour through tmux.
