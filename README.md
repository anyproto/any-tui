# any-tui

A small terminal client for the chats in your [any](https://github.com/anyproto/any)
spaces. It lists every chat in every space, streams new messages live, and lets
you read, search, reply and talk to your [anybao](https://github.com/anyproto/anybao)
agent from a terminal — including over ssh from a phone.

It feels like an IRC client where it can: `/me`, `/dm` and `/msg`,
`s/typo/fix/`, highlight words, nick colours and member icons, a compact
one-line-per-message layout. Beyond that: direct messages, whispers (a private
note on a group message, for one person), search across one chat or every
space, each chat's files and links, attachments opened or saved on Linux and
macOS, and how every space is syncing — through sync nodes, the local network,
or directly peer-to-peer.

It talks only to the local `any` server's REST API (`http://127.0.0.1:7001/v1`
by default). No SDK, no direct store access.

```
╭ chats (2) ─────────────────────╮╭ tui-test/General ────────────────────────────────╮
│tui-test                 lan p2p││  ── beginning of chat ──                         │
│▌ General                       ││★ Ann  21 Sep 00:11                               │
│   ✦ bao: hello from qwen       ││▌ hello from any-tui check                        │
│                                ││▌ 🔒 Bob → you: is this still true?               │
│Bob                      lan p2p││▌ 🔒 you → Bob: yes, still true                   │
│  General                    (2)││  second message via curl (live SSE check)        │
│   ★ Ann: 🔒 yes, still true    ││  third message typed in the TUI                  │
│                                ││                                                  │
│bob-test                 lan p2p││★ Ann  21 Sep 00:25                               │
│  General                       ││  fourth: after the id-in-owners filter           │
│   no messages                  ││                                                  │
│                                ││★ Ann  21 Sep 00:38                               │
│bao                             ││  hello from qwen                                 │
│  General                       │╰──────────────────────────────────────────────────╯
╰────────────────────────────────╯  i  compose   r  reply   ?  help
 NORMAL   new: +1  ↑1/7         ✓ 3 nodes · 1 lan · 1 p2p  7 msgs · 0 files · 0 links
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

Then run the TUI. With Nix, straight from the flake:

```sh
nix run . -- --no-auto-read                # args after `--` go to any-tui
nix run github:anyproto/any-tui            # or without a checkout
nix build && ./result/bin/any-tui          # or build it once
```

or with cargo:

```sh
cargo build --release                      # or: nix develop -c cargo build --release
./target/release/any-tui                   # --api <url> to point elsewhere
```

A space shows up as soon as it has its general chat installed, which any
client that creates spaces does (`POST /v1/catalog/general-chat/setup
{"spaceId": …}`, idempotent). The TUI never creates spaces or chats on its
own; the one exception is a DM you open with `/dm`, which sets up that 1-1
space and its chat.

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
the TUI shows it as `📎 credential_request` and cannot fill it in, so put the
key in `.connectors.env` and restart `serve`.

### Running bao on OpenRouter (GLM 5.3, Qwen 3.8 Max)

No Anthropic key needed: one OpenRouter key and three `llm.tier.*` rows.
The committed `anybao.toml` is the prod default, so keep your copy in the
gitignored `configs/` dir and put the key beside it — `anyrt` reads the
`.connectors.env` next to whichever config file it loads:

```sh
mkdir -p configs && cp anybao.toml configs/anybao.toml
echo 'llm.key.openrouter=sk-or-…' > configs/.connectors.env
cat >> configs/anybao.toml <<'TOML'

[config]   # hard seeds: written into the bao space on every serve start
"llm.tier.codegen"  = { provider = "openai-compat", model = "qwen/qwen3.8-max", base_url = "https://openrouter.ai/api/v1", api_key_ref = "llm.key.openrouter" }
"llm.tier.classify" = { provider = "openai-compat", model = "qwen/qwen3.8-max", base_url = "https://openrouter.ai/api/v1", api_key_ref = "llm.key.openrouter" }
"llm.tier.vision"   = { provider = "openai-compat", model = "qwen/qwen3.8-max", base_url = "https://openrouter.ai/api/v1", api_key_ref = "llm.key.openrouter" }
TOML
./runtime/target/release/anyrt serve --config-file configs/anybao.toml
```

For GLM 5.3 use `z-ai/glm-5.3` in `codegen` and `classify`, and
`z-ai/glm-5v-turbo` in `vision` (GLM 5.3 itself is text-only). Any-ui's
Settings ▸ Model writes the same rows, but the `[config]` table wins on every
restart, so drop it if you want to switch models from the UI. The `.connectors.env`
sibling file must sit in the same directory as the config file passed with `--config-file`.

### Watching bao work

While a run is live the bottom bar shows what bao is doing, fed by the
runtime's presence beats on the any event bus — the same `bao.status` beats
any-ui's status bar reads: bao's own status line when it has set one, else
the code of the cell it is running right now, else the run title, with the
tool-call count in brackets:

```
 NORMAL   ✦ rows = c.query_objects("tui-test", filter={"any.… (3)   no unread elsewhere
```

`✦ bao` alone means idle, `✦ bao offline` that no serve has beaten for 30s,
`✦ no active bao` that only standby devices are up. Nothing shows until the
first beat (≤10s after `serve` starts). The cell preview updates on every
tool call, on every device of the account — the beats ride any-sync, so a
serve on another machine shows up the same way.

To see the whole flow — every effect, HTTP call and model turn, as it
happens — follow the run from a second terminal:

```sh
anyrt trace follow --program toolcaller        # newest chat run, live; exits when it completes
anyrt trace ls --program toolcaller            # past runs: status, duration, turns
anyrt trace show run_<id>                      # one run in full;  --stats for tokens and cost
```

All three take `--addr` (default `http://127.0.0.1:7001`) and `--space`
(default `bao`). Runs are in the any local store, so they follow the account
across devices like everything else.

## Keys

Navigation is vim-flavoured; `?` shows this list in the app, `q` quits.

| key | action |
| --- | --- |
| `j` / `k` | move; in the list each chat opens as you land on it |
| `Enter` or `l` / `Esc` or `h` | step into the open chat / back to the list |
| `Space` | fuzzy-find any chat (`stg` finds **s**ync **t**eam: **g**eneral) |
| `Ctrl-n` / `Ctrl-p` | next / previous chat without leaving the message pane |
| `n` | jump to the next chat with unread, wherever it is |
| `Ctrl-d` / `Ctrl-u`, `g` / `G` | half a screen down / up; oldest (loads history) / newest |
| `Ctrl-v` / `Alt-v`, `PgDn` / `PgUp` | a screenful down / up — in the messages, the chat list, search, the files/links list, the picker and help |
| `i`, then `Enter` | compose, send (`Alt-Enter` for a newline) |
| `r` | reply to the message under the cursor |
| `e` | edit your message under the cursor (`Enter` saves, `Esc` cancels) |
| `@Name<Tab>` | complete a mention from the space's roster |
| `/` | search messages — see [Search](#search) |
| `R` | mark the chat read now |
| `D` | open a DM with the author of the message under the cursor |
| `W` | whisper about the message under the cursor; on a whisper in a DM, `Enter` jumps to the message it's about |
| `o` / `s` | open / save the attachments of the message under the cursor |
| `F` / `L` | the chat's files / links, each with the message it came in (`Tab` switches, `Enter` jumps to the message, `o`/`s` open or save) |
| `z` | hide / show the chat list |
| `C` | compact, IRC-log layout: one line per message and per chat (also `/compact`) |
| `Tab` | switch pane |
| `Ctrl-L` | repaint the screen |

The composer and the picker query support readline editing (`Ctrl-a`/`Ctrl-e`,
`Alt-b`/`Alt-f`, `Ctrl-w`, `Ctrl-u`/`Ctrl-k`, …).
In the composer `↑` / `↓` recall lines you sent before.

Unread shows as a count after the chat, `(2)` — `(@2)` in red when
something unread mentions you, `(★2)` in violet for a highlight word, capped
at `(999+)` — the same in the list, the picker and the status bar.

The status bar holds, left to right: the mode, your away marker, a waiting DM
request or invite (`✉`), a running download, bao's presence, unread in other
chats — and at the right end how the open chat's space is syncing and its
size: `✓ 3 nodes · 1 lan · 3 p2p · 1 own  238 msgs · 26 files · 34 links`.
`✓` is synced (`⟳ 3/5` while syncing, `✗` offline); then the live paths — sync
nodes, other people's devices on the local network (`lan`) or directly over
the internet (`p2p`, any's iroh layer), and your own other devices (`own`),
which sync every space you have. Only peers that sync *that* space count. The
chat list carries the same per space: a `lan` / `p2p` badge on the space (or
DM) header while someone else's device syncs it directly, `✗` when it's
offline. `/devices` lists your own devices.

Below 80 columns the app shows one pane at a time — list or chat — and the
open chat's title becomes a `‹ Esc` breadcrumb. `--layout single|split|auto`
overrides the width check. The bottom bar always lists unread activity in the
chats you are *not* looking at.

## Commands

The composer takes IRC-style commands — `i` opens it even with no chat open,
for `/dm`, `/join`, `/accept` and the like. An unknown `/word` is refused
rather than sent, and `//text` sends a literal leading slash; `/usr/bin`-like
text goes out as-is.

| command | does |
| --- | --- |
| `/me waves` | an action, shown as `* Name waves` (sent as the literal `/me …` text, so other clients show it verbatim) |
| `/shrug`, `/tableflip`, `/unflip` `[text]` | the text plus a face |
| `s/old/new/` (`…/g` for all) | edit your newest message in this chat |
| `/dm @name` or `/dm <identity>` | open (or start) a direct chat; bare `/dm` = the author under the cursor, same as `D` |
| `/msg @name text` | send into that DM without leaving the current chat |
| `/w @name text` | whisper about the message under the cursor — see [Whispers](#whispers) (`W` starts one to its author) |
| `/accept [name]` | accept an incoming DM request or space invite (the status bar shows `✉` while one waits) |
| `/join <chat>` (`/j`) | open the best fuzzy match, like `Space` |
| `/hl [word]`, `/unhl word` | list / add / remove highlight words: others' messages containing one get the word lit up (violet) and the chat's count reads `(★2)` |
| `/away [emoji]`, `/back` | show an away marker (default 💤) in your status bar |
| `/compact` (`C`) | toggle an IRC-log layout: one `HH:MM Name text` line per message with per-day dividers, and one line per chat in the list (`🧉 Gustavo   p2p (2)`) |
| `/icons [safe\|off\|full]` | how icons and emoji are drawn — see [Emoji and odd terminals](#emoji-and-odd-terminals); bare, the next mode |
| `/nick <name>` (`/name`) | rename yourself (keeps your description and icon); bare, it shows your current name and short id |
| `/devices` | your account's devices: this one, which are online (and over what) or when last seen, which one runs bao |
| `/help` | the command list |

A message that opens with a bracketed tag, IRC-style — `[Narrator] And then…`
— shows the tag as a bold label in its own colour (the same tag always gets
the same one). Markdown links like `[text](url)` don't count.

Every name carries the first 7 characters of its identity,
`tolya 🌴 (A7hQ66M)`, so two people who picked the same name stay apart.
Nicks get a stable colour hashed from the identity, and the member's profile
icon in front when a terminal can draw it — an emoji (`🧉 Gustavo`) or a
common icon-pack glyph as a Unicode stand-in (`★ Ann`); picture avatars aren't
shown. Spaces get theirs the same way in the chat list, in the icon's colour
(`🪐 Anytwo Assembly`, `↗ Any Team`), and a DM shows the other person's icon. Highlights, away, compact, icons and the recall history persist in the
daemon's device-local store (`/v1/local`, collection `any_tui`) — they don't
sync to your other devices, and `/away` is not visible to anyone else yet.

## Emoji and odd terminals

Terminals, tmux and mosh each keep their own table of character widths, and
they disagree on emoji newer than Unicode 9 (2016) and on composed ones
(`🤦‍♀️`, `❤️`, skin tones): the cursor drifts, text lands a column off, and
stray characters stay on screen. The TUI can't learn what the far end
decided, so it avoids sending what they disagree on. `/icons` picks how:

| mode | icons | emoji |
| --- | --- | --- |
| `safe` (default) | shown, unless the icon is a newer emoji | older ones as they are, composed ones reduced to their base, newer ones as `◌` |
| `off` | hidden | as text: the common reactions two columns wide (`👍` `+1`, `❤` `<3`, `😂` `:D`), the rest `◌` — a pure IRC look with `C` |
| `full` | shown | everything as sent — fine on a local terminal without tmux or mosh |

`Ctrl-L` repaints the whole screen if anything is left over.

## Direct messages

A DM is a 1-1 space shared by the two of you, and it sits in the chat list
like any other space, named after the other person. `/dm @name` (anyone the
server knows a name for, not just this space's members), `/dm <identity>`, or
`D` on someone's message opens it — starting it if it doesn't exist yet. Until
they accept, it's listed as `@` plus their short id, since their name only
becomes readable then. On the receiving side a request shows as
`✉ DM from Ann (/accept)` in the status bar until `/accept` opens it; being
added to a space shows the same way (`✉ invite: …`). `/msg @name text` sends
into a DM without leaving the chat you're in.

## Whispers

A whisper is a private side-note on a message in a group chat, for one person.
Put the cursor on the message and press `W` (or type `/w @name text`). It is
sent into your DM with that person as the message link plus your text, so
nobody else in the group can see it, and any-ui shows it as an ordinary DM
with a link. In the TUI both of you see it under the original message, on
a dark red band headed `🔒 WHISPER · only you and Ann can see this`; in the
DM it opens with the same red `🔒 whisper about …` line, and `Enter` on it
jumps back to the message.

Replies to your own messages stand out too: the quote reads
`↪ reply to you: …` in bold yellow. Other messages that mention you are
marked `@you` (on the line itself in compact mode).

## Search

`/` turns the message pane into a search view. The border shows where and how
you're searching as two switches: **this chat │ this space │ all spaces**
(`Tab` widens, `Shift-Tab` narrows) and **hybrid │ fts** (`Ctrl-t`; hybrid
mixes keyword and semantic ranking, fts matches the words exactly; `Ctrl-g`
turns on semantic-only). Results update as you type, best match at the bottom
by the prompt like fzf; `Ctrl-o` flips to newest-first. Matched words are
highlighted, long messages are cut to the lines around the match (the one
under the cursor opens up), and `from:@name` keeps one person's messages.
`Enter` jumps to the message in its chat, `Ctrl-r` jumps and replies. The
index only knows content written after it was enabled, and semantic ranking
needs the server's embedder — the status bar says when it wasn't used.

## Attachments

A message's files show as `📎 name · size`, one per line. `o` downloads them
into a cache and opens each with your default app (`xdg-open` on Linux, `open`
on macOS); `s` saves them to your Downloads folder — never overwriting, a
clash becomes `name (1).ext` — and shows them in the file manager (Finder, or
whichever answers the freedesktop `FileManager1` call; else the folder opens).
Progress shows on the attachment line and in the status bar. Web-link
attachments open in the browser. Sending files isn't supported yet.

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
| `--version` | print the version |

## More

- [`docs/api-notes.md`](docs/api-notes.md) — how the client maps onto the
  any API (chat discovery, `_ver.id` paging, mentions, read state, SSE
  contract, search, DMs and whispers, files, sync status, the local store)
  and the layout of the code.
- `scripts/api-drift.sh [url]` — diffs a running server's OpenAPI spec against
  the one pinned in `api/openapi.json`; `--update` re-pins.
- `cargo test` — unit tests for the pure logic: record and link parsing
  (mentions, whispers, URLs, icons, sync status), the fuzzy matcher, the
  command parser and the attachment file handling.

## License

[MIT](LICENSE).
