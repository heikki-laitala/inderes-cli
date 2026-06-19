---
name: inderes
description: Research Finnish/Nordic equities via the Inderes research platform — company fundamentals, analyst estimates, research reports, earnings-call transcripts, calendar events, insider trades, forum posts. Invoke when the user asks about publicly-traded Nordic companies, Finnish stocks, analyst coverage, or Inderes itself.
metadata:
  {
    "openclaw":
      {
        "emoji": "📈",
        "requires": { "bins": ["inderes"] },
      },
  }
---

# Inderes

`inderes` is a CLI that talks to Inderes's MCP server on the user's behalf. Use it to answer questions about Finnish/Nordic equity research: fundamentals, analyst estimates, research reports, earnings calls, insider trades, and Inderes's own model portfolio.

The user must have run `inderes login` once (browser-based OAuth). If a call errors with "not signed in", tell the user to run `inderes login` — do not attempt to authenticate on their behalf.

## Usage pattern

Always **resolve the company ID first** via `inderes search`. Every other tool takes an opaque ID like `COMPANY:200`, not a ticker or name.

```bash
inderes search "Nokia"
# -> list of matches with COMPANY:<id>
```

Then call one of the friendly subcommands, or drop down to `inderes call <tool>` for anything rarer.

## Friendly subcommands

| Subcommand | Answers |
|---|---|
| `inderes search <query>` | "Which company ID is …?" |
| `inderes fundamentals <id> --field revenue --field ebitda --from-year 2020` | Historical income, margins, multiples |
| `inderes estimates <id> --field revenue --field eps --quarters --years 3` | Forward analyst estimates + recommendations |
| `inderes content list --company-id <id> --type COMPANY_REPORT --first 10` | Recent research notes and articles |
| `inderes content get <contentId-or-URL>` | Fetch one article/report body |
| `inderes documents list <id>` | Annual/interim filings issued by the company |
| `inderes documents get <documentId>` | TOC of a filing |
| `inderes documents read <documentId> -s 1,2,5` | Read specific sections |

Pass `--json` to any of the above to get raw JSON (useful when you need to extract structured data).

## Forum (public, no login)

`inderes forum` reads the public Inderes forum (forum.inderes.com) directly — **no `inderes login` required**; it sends no credentials. Use it for community/retail sentiment and discussion, as distinct from analyst research.

```bash
inderes forum search "Nokia"      # full-text search across topics and posts
inderes forum topic 74            # full thread (read-through SQLite cache)
inderes forum topic 74 --refresh  # re-fetch from page 1, updating edits
inderes forum latest              # latest active topics
inderes forum categories          # category list
inderes forum query "<SQL>"       # read-only SQL over the cached posts
inderes forum db-path             # path to the local SQLite cache
```

Add `--json` for raw Discourse fields (`cooked` = post body HTML, `username`, `created_at`) when extracting post text for analysis. `forum topic` returns the **whole** thread, backed by a local SQLite read-through cache: only new posts are fetched each call, and the full thread is served from disk (first call on a huge thread is slow but resumable; re-run if interrupted). This is separate from the authenticated MCP tool `inderes call search-forum-topics`. Cached posts are queryable with `inderes forum query "<SQL>"` (read-only) — e.g. most active users on a stock, or posting volume over time.

## Analyzing cached threads (you reason, the CLI only serves data)

The cache has a clean-text `text` column (HTML stripped) — query `text`, not `cooked`, to save tokens. Cache a topic first with `inderes forum topic <id>`, then:

- **Catch me up** (recent posts): `inderes --json forum query "SELECT post_number, username, date(created_at) d, text FROM posts WHERE topic_id=74 ORDER BY post_number DESC LIMIT 40"` → summarize the result yourself.
- **Summarize a long thread** (map-reduce — a big thread won't fit in context): pull it in chunks, summarize each, then combine. Get the size with `SELECT MAX(post_number) FROM posts WHERE topic_id=74`, then `... WHERE topic_id=74 AND post_number BETWEEN 1 AND 500 ORDER BY post_number` (repeat for 501–1000, …).
- **Sentiment / bull-vs-bear**: classify a representative slice — the posts the thread reacted to, plus the latest: `inderes --json forum query "SELECT username, text, json_extract(raw,'$.score') score FROM posts WHERE topic_id=74 ORDER BY score DESC LIMIT 50"`.

The CLI never summarizes or scores — it returns rows; you do the judgment with your own model.

## Escape hatch: all 16 tools

The server exposes more tools than the friendly subcommands wrap. To see everything:

```bash
inderes call --list
```

Then call any tool directly:

```bash
inderes call list-transcripts --arg companyId=COMPANY:200 --arg first=10
inderes call get-transcript --arg transcriptId=TRANSCRIPT:VIDEO:19187 --arg lang=en
inderes call list-calendar-events --arg 'regions=["FINLAND","SWEDEN"]' --arg first=20
inderes call list-insider-transactions --arg companyId=COMPANY:200
inderes call search-forum-topics --arg text=Nokia --arg order=relevancy
inderes call get-model-portfolio-content
```

`--arg KEY=VALUE` parses VALUE as JSON when possible (numbers, booleans, arrays, objects, quoted strings); otherwise treats it as a plain string. For a full JSON object pass `--json-args '{"key":"value"}'`.

## Guidance for agents

- **Start with `search`.** Don't guess COMPANY IDs.
- **Prefer friendly subcommands.** Only drop to `inderes call` for tools the friendly set doesn't cover.
- **Use `--json` when you need to post-process** (e.g. extract numbers). Default output is already compact text.
- **Paginate when results exceed expected size.** Most list tools expose `--first` and `--after`; grab cursors from `pageInfo.endCursor` in `--json` output.
- **Respect language.** `inderes content get --lang en` when the user wants English; `fi` is the default for Finnish content.
- **Do not try to refresh tokens manually** — the CLI does it. If it says "sign in expired", ask the user to run `inderes login`.

## Data scope

Inderes primarily covers Finland and other Nordics (Sweden, Denmark, Norway, Estonia). Some tools also support France, Germany, USA via the `regions` parameter.

This CLI is an unofficial community wrapper. It requires the user's own Inderes Premium subscription — the CLI never bypasses auth.
