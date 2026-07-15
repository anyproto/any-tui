# any-tui

A terminal chat reader for [any](https://github.com/anyproto/any) spaces. Reads
every chat in every space, streams new messages live, and keeps a status bar of
unread activity in the chats you're *not* looking at.

Talks only to the local any REST API (`http://127.0.0.1:7001/v1` by default) —
no SDK, no direct store access.

```
cargo run --release
cargo run --release -- --api http://127.0.0.1:7001/v1 --no-auto-read
```

The any daemon must be running and authorized (`GET /v1/health` returns an
`account`).

## Layout

```
╭ chats (1) ──────────────────╮╭ sync team/general ──────────────────────╮
│sync team                    ││Konstantin Ivanov  11 Jul 12:29          │
│▌● general                  3││  ↪ tolya: посмотрите плиз…              │
│   tolya: Кость, аппка уста… ││  я посмотрел - все ок                   │
│ ○ space chat                ││  👍1                                     │
│   km: отправил второй инвайт││                                         │
╰─────────────────────────────╯╰─────────────────────────────────────────╯
 NORMAL   new: ●dev/general 2  ●Alex/general 1              ↑8 lines
```

Chats are grouped by space and ordered by most recent activity. Every row shows
a preview of the last message — spaces routinely contain several chats all named
`general`, and the preview is what actually tells them apart. The unnamed,
nav-less chat object in each space is shown as `space chat`.

## Keys

| key | action |
| --- | --- |
| `j` / `k`, `↓` / `↑` | move selection (chats) · scroll (messages) |
| `Tab` | switch pane |
| `Enter` | open selected chat |
| `n` | jump to next chat with unread |
| `g` / `G` | oldest (loads history) / newest |
| `Ctrl-d` / `Ctrl-u` | half page down / up |
| `i` | compose (`Esc` cancels, `Enter` sends) |
| `r` | reply to newest message |
| `R` | mark chat read now |
| `?` | help |
| `q` / `Ctrl-c` | quit |

Opening a chat marks it read once the newest message is on screen. Pass
`--no-auto-read` to read without touching read state.

## How it maps onto the API

Worth writing down — most of this is not obvious from the swagger.

**Discovering chats.** There is no "list chats" endpoint. Chat objects are
ordinary objects carrying `any.types: ["chat"]`, found per space via
`POST /spaces/{id}/objects/query` with `{"filter": {"any.types": "chat"}}`.
Each record's `chat.unreadCount` / `chat.unreadReactionsCount` drive the badges,
so the objects subscription doubles as the unread feed. Spaces typically hold
one unnamed chat (no `nav`) plus named chats in the nav tree.

**Messages** live in the per-object `chat_messages` dataset, read via
`POST /spaces/{id}/query` with `{objectId, dataset, sort, limit, offset}`.

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
| `app.rs` | state, event enum, subscription tasks |
| `ui.rs` | rendering, wrapping, scroll geometry |
| `main.rs` | CLI, terminal lifecycle, keymap, event loop |

One subscription runs per space for the whole session (unread + chat list); a
second is opened for the chat currently on screen and aborted when you switch.
