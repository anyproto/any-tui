# any-tui

A terminal chat reader for [any](https://github.com/anyproto/any) spaces. Reads
every chat in every space, streams new messages live, and keeps a status bar of
unread activity in the chats you're *not* looking at.

Talks only to the local any REST API (`http://127.0.0.1:7001/v1` by default) —
no SDK, no direct store access.

```sh
cargo run --release
```

The any daemon must be running and authorized (`GET /v1/health` returns an
`account`).

---

## Manual

### Starting out

You land on the **chat list**, grouped by space. Move with `j`/`k` — each chat
opens as you land on it, no `Enter` needed. `Enter` steps *into* the chat to
scroll and reply; `Esc` steps back out. Press `?` for the keymap, `q` to quit.

Three ways to get around, in rough order of how often you'll want them:

| | |
| --- | --- |
| `Space` | **fuzzy-find any chat** — the fastest way when you know where you're going |
| `Ctrl-n` / `Ctrl-p` | next / previous chat in list order, **without leaving the chat you're reading** |
| `j` / `k` | walk the list, previewing as you go |

Every row shows a preview of its last message:

```
sync team
▌○ general
   tolya: Кость, аппка установилась! Спасибо! Ча…
 ○ space chat
   km: отправил второй инвайт
```

The preview is load-bearing, not decoration: a space usually contains several
chats **all named `general`**, and the preview is the only thing that tells them
apart. `●` means unread, `○` read; the count sits on the right. Chats are ordered
by most recent activity within each space, so live ones float to the top.

### Finding a chat: `Space`

`Space` opens a centred fuzzy picker over every chat, in the style of helix's
file/buffer menus:

```
╭ chats (4) ───────────────────────────────────────╮
│  > stg                                           │
│                                                  │
│▌ sync team: general                              │
│    tolya: Завел третий мобильный клиент.         │
│  sync team: general                          ● 3 │
│    km: Толя, Серега, гляньте плиз ПРы…           │
╰──────────────────────────────────────────────────╯
```

Type to filter — matching is fuzzy over `space: chat`, so `stg` finds
**s**ync **t**eam: **g**eneral and matched letters are highlighted. Rows carry
the last-message preview and any counters (`● 3` unread, `♥ 1` reactions).

| key | action |
| --- | --- |
| `Ctrl-n` / `Ctrl-p`, `↓` / `↑` | move (wraps around) |
| `Enter` | open it |
| `Ctrl-u` | clear the query |
| `Esc` | dismiss |

### Searching messages: `/`

`/` (while a chat is open) turns the message pane into a **full-text / semantic
search** over chat messages. The query lives where the composer usually sits;
results appear above it, oldest→newest, enriched with sender and time and
navigable exactly like a chat:

```
╭ search ──────────────────────────────────────────╮
│ tolya  10 Jul 15:55  · bao/general               │
│   create a comic book type in the foo space…     │
│▌tolya  10 Jul 16:02  · bao/general               │
│▌ Good — got the confirmed fix. Now building it.  │
╰──────────────────────────────────────────────────╯
╭ / ───────────────────────────────────────────────╮
│> comic book                                      │
╰ Enter open · Ctrl-r reply · Tab scope · Esc ─────╯
 SEARCH  space: bao · hybrid · semantic  7 hits
```

Typing searches after a short pause (results update as you go). The query is
always live, so navigation and actions are on `Ctrl`/arrow keys:

| key | action |
| --- | --- |
| type | edit the query (full readline editing) |
| `↓` / `↑`, `Ctrl-n` / `Ctrl-p` | move the result cursor |
| `PgDn` / `PgUp` | move by a page |
| `Tab` | cycle **scope**: this chat → this space → all spaces |
| `Ctrl-t` | cycle **mode**: hybrid → fts → vector |
| `Enter` | jump to the message in its real chat |
| `Ctrl-r` | jump there **and** start a reply |
| `Esc` | close search |

The status bar shows the active **scope**, the **mode** that actually ran, and
whether semantic recall participated (`semantic` / `keyword only` / `semantic
offline`). **Scopes** map onto the per-space search endpoint: `chat` filters the
current space's hits to the open chat, `space` searches the whole space, and
`all spaces` fans the query out across every space.

You can filter by sender with a **`from:@name`** token anywhere in the query
(e.g. `deploy from:alice`) — the search engine has no sender filter, so it's
stripped out and applied to the results.

Two things worth knowing:

- The index is **forward-only**: messages written before the daemon started
  indexing won't be found (see `docs/13-index.md` in `any`).
- There's no result pagination — you get the top 100 by relevance, re-sorted by
  time. Narrow the query or scope if what you want isn't there.

### Narrow screens (mobile tmux)

Under **80 columns** the app shows **one pane at a time** — the chat list, or the
open chat, never both. This is automatic; there's nothing to configure.

```
list ──Enter──▶ chat
     ◀──Esc────
```

| you want | press |
| --- | --- |
| open the chat under the cursor | `Enter` |
| get back to the chat list | `Esc` (or `h`, `←`, `Backspace`) |
| force two panes anyway | `z` |
| force one pane on a wide screen | `z` |

`z` flips between one and two panes relative to what's on screen and **pins**
your choice, so auto stops overriding it. The open chat's title doubles as a
breadcrumb and reminds you of the way out:

```
╭ ‹ Esc  sync team/general ────────────────────────╮
```

To pick a layout up front, skipping auto entirely:

```sh
any-tui --layout single   # always one pane
any-tui --layout split    # always two
any-tui --layout auto     # default: one under 80 cols, two above
```

**The bottom bar always survives.** Even with the chat list hidden, it keeps
listing unread elsewhere, and it budgets itself against the real width — on a
narrow screen it shows space names (`●dev 2`) rather than `●dev/general 2`,
because "general" tells you nothing:

```
 NORMAL   new: ●dev 2  ●Alex 1                    ↑8 lines
```

### Reading

Inside a chat, `j`/`k` move a **message cursor**, drawn as an accent bar down
the left of the message it's on:

```
tolya  07:13
▌ #105 — появилось ощущение что в any стало все в кучу
▌ 📎 2 images
  надо подумать, но тема нужная
```

The cursor matters because it's what `r` replies to. Consecutive messages from
one author are grouped under a single header, so "the last message" is often
ambiguous — the bar removes the guesswork.

| key | action |
| --- | --- |
| `j` / `k`, `↓` / `↑` | move the message cursor (or the selection in the list) |
| `Ctrl-n` / `Ctrl-p` | next / previous chat, without leaving the message pane |
| `Ctrl-d` / `Ctrl-u` | jump 5 messages |
| `G` | jump to newest |
| `g` | jump to oldest — pulls in older history as you go |
| `n` | jump to the next chat with unread, wherever it is |
| `Tab` | switch pane / swap which pane is visible |

Messages carrying files show `📎 2 images` under the text. Attachments can't be
opened or sent yet — this only tells you they're there.

**Agent messages** (written by an AI agent acting in the space) are marked with
a `✦` and the agent's name in a distinct colour — the daemon signs them with the
human account, so without this they'd read as that person. While an agent is
still working, its newest message shows a `✦ <name> is working…` line, live over
SSE. The sidebar preview and search results use the agent's name too.

New messages arrive live over SSE — no polling, no refresh key. If you're
scrolled up reading history, an arriving message **won't yank you to the
bottom**; the viewport stays where you put it and `↑N` in the status bar shows
how far up you are.

A `── new ──` rule marks where your unread starts.

### Writing

| key | action |
| --- | --- |
| `i` | compose |
| `Enter` | send |
| `Alt-Enter` (or `Ctrl-j`) | newline — `Enter` is taken by send |
| `r` | reply to the message under the cursor |
| `R` | mark this chat read right now |
| `Esc` | cancel |

The composer supports the usual **readline editing** — a movable cursor, not
just append-and-backspace. The same keys work in the `Space` picker's query:

| key | action |
| --- | --- |
| `←` / `→`, `Ctrl-b` / `Ctrl-f` | move by character |
| `Ctrl-←` / `Ctrl-→`, `Alt-b` / `Alt-f` | move by word |
| `Ctrl-a` / `Ctrl-e`, `Home` / `End` | start / end |
| `Backspace`, `Delete` (`Ctrl-d`) | delete char before / after |
| `Ctrl-w` (`Alt-Backspace`), `Alt-d` | delete word before / after |
| `Ctrl-u` | clear to start · `Ctrl-k` clear to end |

The composer wraps at word boundaries and **grows as you type**, up to 8 lines,
then scrolls. Replying pins a banner above it naming exactly who and what
you're answering, so the target stays visible while you type:

```
 ↩ replying to km: Толя, Серега, гляньте плиз ПРы: 1. https://gi…
╭ message ─────────────────────────────────────────╮
│reply text that is long enough to wrap here       │
╰ Enter send · Alt-Enter newline · Esc cancel ─────╯
```

### Read state

Opening a chat marks it read once the newest message is actually on screen —
scrolled to the bottom, not merely opened. It never marks read while you're
scrolled up in history.

"On screen" means the **message cursor is on the newest message** — reading
history never clears unread. Because the cursor previews chats as it moves,
marking read also waits for you to **stay put for ~1.5s**: walking `j`/`k` past
a stack of unread chats, or cycling with `Ctrl-n`, leaves them unread. In
one-pane mode a chat loaded behind the list is never marked read, since you
haven't seen it. Read state has no undo in the API, so the bias is always
towards *not* marking.

```sh
any-tui --no-auto-read    # read without ever touching read state
```

There is no un-read endpoint in the API, so this is one-way: use
`--no-auto-read` if you want to keep unread badges for another client.

---

## How it maps onto the API

Worth writing down — little of this is obvious from the swagger.

**Discovering chats.** There is no "list chats" endpoint, and (since the
2026-09 server) no `"chat"` type either. A space has one chat, the general
chat, and its object is a bundle root that is its own type: `any.types` holds
the root's own id, `type.layout` is `{"type": "chat"}`, and it is named
`General`. The chat-declaring type ids are the `owners` of the `chat_messages`
dataset in `GET /spaces/{id}/datasets`; the client matches `any.types` against
those with `$in`, OR-ed with `type.layout.type == "chat"` and the legacy
literal `"chat"` so older daemons still work. Each record's `chat.unreadCount`
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
