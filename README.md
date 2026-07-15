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

You land on the **chat list**, grouped by space. Move with `j`/`k`, open with
`Enter`, get back with `Esc`. Press `?` at any time for the keymap, `q` to quit.

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

| key | action |
| --- | --- |
| `j` / `k`, `↓` / `↑` | scroll messages (or move selection in the list) |
| `Ctrl-d` / `Ctrl-u` | half page down / up |
| `G` | jump to newest |
| `g` | jump to oldest — pulls in older history as you go |
| `n` | jump to the next chat with unread, wherever it is |
| `Tab` | switch pane / swap which pane is visible |

New messages arrive live over SSE — no polling, no refresh key. If you're
scrolled up reading history, an arriving message **won't yank you to the
bottom**; the viewport stays where you put it and `↑N` in the status bar shows
how far up you are.

A `── new ──` rule marks where your unread starts.

### Writing

| key | action |
| --- | --- |
| `i` | compose (`Enter` sends, `Esc` cancels) |
| `r` | reply to the newest message |
| `R` | mark this chat read right now |

### Read state

Opening a chat marks it read once the newest message is actually on screen —
scrolled to the bottom, not merely opened. It never marks read while you're
scrolled up in history.

```sh
any-tui --no-auto-read    # read without ever touching read state
```

There is no un-read endpoint in the API, so this is one-way: use
`--no-auto-read` if you want to keep unread badges for another client.

---

## How it maps onto the API

Worth writing down — little of this is obvious from the swagger.

**Discovering chats.** There is no "list chats" endpoint. Chat objects are
ordinary objects carrying `any.types: ["chat"]`, found per space via
`POST /spaces/{id}/objects/query` with `{"filter": {"any.types": "chat"}}`.
Each record's `chat.unreadCount` / `chat.unreadReactionsCount` drive the badges,
so the objects subscription doubles as the unread feed. Spaces typically hold
one unnamed chat with no `nav` (shown as `space chat`) plus named chats in the
nav tree — often several, all called `general`.

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
| `ui.rs` | rendering, wrapping, scroll geometry, pane layout |
| `main.rs` | CLI, terminal lifecycle, keymap, event loop |

One subscription runs per space for the whole session (unread + chat list); a
second is opened for the chat currently on screen and aborted when you switch.
