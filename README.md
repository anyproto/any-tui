# any-tui

A small terminal client for the chats in your [any](https://github.com/anyproto/any)
spaces. It lists every chat in every space, streams new messages live, and lets
you read, search, reply and talk to your [anybao](https://github.com/anyproto/anybao)
agent from a terminal — including over ssh from a phone.

It talks only to the local `any` server's REST API (`http://127.0.0.1:7001/v1`
by default). No SDK, no direct store access.

```
╭ chats ─────────────────────╮╭ bao/General ──────────────────────────────────╮
│tui-test                    ││A9fBcRQu  00:32                                │
│ ○ General                  ││  hi, what can you do? answer in one sentence  │
│   A9fBcRQu: fourth: after… ││                                               │
│                            ││✦ bao  00:32                                   │
│bao                         ││▌ I keep your space organized and get things   │
│▌○ General                  ││▌ done in it: pages, notes, tasks and types,   │
│   ✦ bao: I keep your spac… ││▌ search across your mail and history, …       │
╰────────────────────────────╯╰───────────────────────────────────────────────╯
 NORMAL   no unread elsewhere
```

## Quick start

You need a running, authorized `any` server. Build it from the
[any repository](https://github.com/anyproto/any) and start it on an empty
data dir:

```sh
any run                                    # http://127.0.0.1:7001, unauthorized
```

A fresh data dir has no account: `GET /v1/health` reports `"account": ""` and
every other route answers `401 auth.required`. Authorize once with curl
(or `any auth login` from the any CLI):

```sh
API=http://127.0.0.1:7001/v1

# new account — the mnemonic is returned ONCE, save it: it IS the account
curl -s -X POST $API/auth -H 'content-type: application/json' -d '{}' | jq

# or restore an existing one (add "index": 0 for an anytype-derived account)
curl -s -X POST $API/auth -H 'content-type: application/json' \
  -d '{"mnemonic": "word word word …"}'

curl -s $API/health | jq .account         # non-empty now
```

The account boots in place, no restart. A standalone server will not switch
accounts once one is up: stop it and run it on another data dir instead.

Then build and run the TUI:

```sh
cargo build --release                      # or: nix develop -c cargo build --release
./target/release/any-tui                   # --api <url> to point elsewhere
```

A space shows up as soon as it has its general chat installed, which any
client that creates spaces does (`POST /v1/catalog/general-chat/setup
{"spaceId": …}`, idempotent). The TUI itself never creates anything.

## Talking to an agent

[anybao](https://github.com/anyproto/anybao) is a personal agent that lives in
your spaces; `anyrt serve` is the process that runs it. Point it at the same
`any` server and it creates a `bao` space with a chat, which the TUI then lists
like any other. From the anybao checkout:

```sh
uv sync && make kernel && make runtime     # builds runtime/target/release/anyrt
echo 'llm.key.anthropic=sk-ant-…' > .connectors.env   # gitignored; seeded on every boot
./runtime/target/release/anyrt serve --addr http://127.0.0.1:7001
```

Wait for `anyrt serving space=… chat=…` in its log (the first boot also
joins the published agent repos, so give it a moment). Now open
`bao/General` in the TUI, `i`, type, `Enter`; the
reply lands over SSE a few seconds later, rendered with a `✦` marker and the
agent's name rather than your own. Other providers (OpenRouter, Ollama, …) are
one config row each — see anybao's
[`docs/llm-models.md`](https://github.com/anyproto/anybao/blob/main/docs/llm-models.md).
If the key is missing, bao posts a credential-request bubble into the chat;
the TUI shows it as `📎 1 credential_request` and cannot fill it in, so put the
key in `.connectors.env` and restart `serve`.

## Keys

Navigation is vim-flavoured; `?` shows this list in the app, `q` quits.

| key | action |
| --- | --- |
| `j` / `k` | move; in the list each chat opens as you land on it |
| `Enter` / `Esc` | step into the open chat / back to the list |
| `Space` | fuzzy-find any chat (`stg` finds **s**ync **t**eam: **g**eneral) |
| `Ctrl-n` / `Ctrl-p` | next / previous chat without leaving the message pane |
| `n` | jump to the next chat with unread, wherever it is |
| `Ctrl-d` / `Ctrl-u`, `g` / `G` | 5 messages at a time; oldest (loads history) / newest |
| `i`, then `Enter` | compose, send (`Alt-Enter` for a newline) |
| `r` | reply to the message under the cursor |
| `@Name<Tab>` | complete a mention from the space's roster |
| `/` | search messages: `Tab` cycles scope (chat → space → all), `Ctrl-t` cycles hybrid / fts / vector, `from:@name` filters by sender, `Enter` jumps to the hit |
| `R` | mark the chat read now |
| `z` | hide / show the chat list |
| `Tab` | switch pane |

The composer and the picker query support readline editing (`Ctrl-a`/`Ctrl-e`,
`Alt-b`/`Alt-f`, `Ctrl-w`, `Ctrl-u`/`Ctrl-k`, …).

Below 80 columns the app shows one pane at a time — list or chat — and the
open chat's title becomes a `‹ Esc` breadcrumb. `--layout single|split|auto`
overrides the width check. The bottom bar always lists unread activity in the
chats you are *not* looking at.

## Read state

Opening a chat marks it read once its newest message has been on screen for
1.5s. The API has no un-read endpoint, so this is one-way. Pass
`--no-auto-read` to read without ever touching read state, for example when
another client should keep its badges.

## Flags

| flag | |
| --- | --- |
| `--api <url>` | server base URL, default `http://127.0.0.1:7001/v1` |
| `--no-auto-read` | never mark anything read |
| `--layout auto\|split\|single` | pane layout, default `auto` |

## More

- [`docs/api-notes.md`](docs/api-notes.md) — how the client maps onto the
  any API (chat discovery, `_ver.id` paging, mentions, read state, SSE
  contract) and the layout of the code.
- `scripts/api-drift.sh [url]` — diffs a running server's OpenAPI spec against
  the one pinned in `api/openapi.json`; `--update` re-pins.
- `cargo test` — unit tests for record parsing and the fuzzy matcher.
