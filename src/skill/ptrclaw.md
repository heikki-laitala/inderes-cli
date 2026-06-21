---
name: inderes
description: Research Finnish/Nordic equities via the Inderes research platform — company fundamentals, analyst estimates, research reports, earnings-call transcripts, calendar events, insider trades, forum posts. Invoke when the user asks about publicly-traded Nordic companies, Finnish stocks, analyst coverage, or Inderes itself.
---

# Inderes

`inderes` is a CLI that talks to Inderes's MCP server on the user's behalf. Use it to answer questions about Finnish/Nordic equity research: fundamentals, analyst estimates, research reports, earnings calls, insider trades, and Inderes's own model portfolio.

Invoke commands through ptrclaw's `shell` tool (e.g. `shell(command="inderes search Nokia")`). The binary is locally installed; no remote MCP registration is needed.

## When to Use

- User asks about a Nordic-listed company (Finland, Sweden, Denmark, Norway, Estonia) by name or ticker.
- User asks for analyst estimates, recommendations, target prices, earnings-call content, or insider transactions.
- User references Inderes (the research house), Inderes model portfolio, or `inderes.fi` articles.
- Anything where Nordic equity research data would materially improve the answer.

Do **not** use it for non-equity questions or for markets outside the Nordics/selected EU/US coverage. Do not attempt to authenticate — the user must have run `inderes login` once. If a command errors with "not signed in", tell the user to run `inderes login` and stop.

## Quick Reference

Always resolve the company ID first; every other tool takes an opaque ID like `COMPANY:200`, not a ticker.

```bash
inderes search "Nokia"
```

Then call a friendly subcommand:

| Subcommand | Answers |
|---|---|
| `inderes fundamentals <id> --field revenue --field ebitda --from-year 2020` | Historical income, margins, multiples |
| `inderes estimates <id> --field revenue --field eps --quarters --years 3` | Forward analyst estimates + recommendations |
| `inderes content list --company-id <id> --type COMPANY_REPORT --first 10` | Recent research notes and articles |
| `inderes content get <contentId-or-URL>` | Fetch one article/report body |
| `inderes documents list <id>` | Annual/interim filings issued by the company |
| `inderes documents get <documentId>` | TOC of a filing |
| `inderes documents read <documentId> -s 1,2,5` | Read specific sections |

Append `--json` on any subcommand to get raw JSON (easier to post-process).

## Forum (via the MCP server, requires login)

`inderes forum` reads the Inderes forum (forum.inderes.com) through the hosted MCP server — `inderes login` (Premium) is required, like every other subcommand. Use it for community/retail sentiment and discussion, as distinct from analyst research.

```bash
inderes forum search "Nokia"      # search topic titles (up to 10 matches)
inderes forum topic 74            # full thread (read-through SQLite cache)
inderes forum topic 74 --refresh  # re-fetch from the start, updating edits
inderes forum query "<SQL>"       # read-only SQL over the cached posts
inderes forum activity 74         # posting volume over time (momentum)
inderes forum momentum           # rank ALL cached topics by momentum
inderes forum topics             # list cached topics
inderes forum refresh-all        # pull new posts for every cached topic
inderes forum clear <id>         # drop one cached topic (--all to wipe)
inderes forum db-path             # path to the local SQLite cache
```

`forum search` wraps the `search-forum-topics` MCP tool (title match, max 10) and prints each thread's id so it feeds straight into `forum topic <id>`. `forum topic` pulls the **whole** thread via the `get-forum-posts` tool into a local SQLite read-through cache: only new posts are fetched each call (resumable — re-run if interrupted), and the full thread is served from disk. Add `--json` for the raw fields (`cooked` = markdown body, `username`, `created_at`, plus `url`/`score`/`reply_count` in `raw`). A thread opened by bare id shows as `(untitled)` — the server returns no thread title; the title appears in `forum search` results. Cached posts are queryable with `inderes forum query "<SQL>"` (read-only) — e.g. most active users on a stock, or posting volume over time.

## Analyzing cached threads (you reason, the CLI only serves data)

The cache has a clean-text `text` column — query `text` for post bodies. Cache a topic first with `inderes forum topic <id>`, then:

- **Catch me up** (recent posts): `inderes --json forum query "SELECT post_number, username, date(created_at) d, text FROM posts WHERE topic_id=74 ORDER BY post_number DESC LIMIT 40"` → summarize the result yourself.
- **Summarize a long thread** (map-reduce — a big thread won't fit in context): pull it in chunks, summarize each, then combine. Get the size with `SELECT MAX(post_number) FROM posts WHERE topic_id=74`, then `... WHERE topic_id=74 AND post_number BETWEEN 1 AND 500 ORDER BY post_number` (repeat for 501–1000, …).
- **Sentiment / bull-vs-bear**: classify a representative slice — the posts the thread reacted to, plus the latest: `inderes --json forum query "SELECT username, text, json_extract(raw,'$.score') score FROM posts WHERE topic_id=74 ORDER BY score DESC LIMIT 50"`.

The CLI never summarizes or scores — it returns rows; you do the judgment with your own model.

## Procedure

1. Parse what the user needs — fundamentals, estimates, a specific report, an event, an insider trade, etc.
2. If you don't already know the `COMPANY:<id>`, run `inderes search "<name>"` and pick the best match. Never guess IDs.
3. Call the most specific friendly subcommand for the task. Prefer a narrow `--field` selection over pulling all fields — large dumps bloat context.
4. For anything outside the friendly-subcommand set, fall through to the escape hatch:

   ```bash
   inderes call --list                                      # show all 16 tools
   inderes call list-transcripts --arg companyId=COMPANY:200 --arg first=10
   inderes call get-transcript --arg transcriptId=TRANSCRIPT:VIDEO:19187 --arg lang=en
   inderes call list-calendar-events --arg 'regions=["FINLAND","SWEDEN"]' --arg first=20
   inderes call list-insider-transactions --arg companyId=COMPANY:200
   inderes call search-forum-topics --arg text=Nokia --arg order=relevancy
   inderes call get-model-portfolio-content
   ```

   `--arg KEY=VALUE` auto-parses JSON values (numbers, booleans, arrays, objects, quoted strings) and otherwise treats them as plain strings. For a full object, use `--json-args '{"key":"value"}'`.
5. Paginate when lists exceed expected size. Most list tools expose `--first` and `--after`; grab the next cursor from `pageInfo.endCursor` in `--json` output.
6. Respect language preferences: pass `--lang en` (or `fi`/`sv`/`da`) to `inderes content get` / `inderes call get-transcript` when the user wants a specific language.

## Pitfalls

- **Guessing COMPANY IDs** leads to silent empty results — always search first.
- **Requesting all fields** on `fundamentals` or `estimates` can produce multi-kilobyte JSON. Pick what you need.
- **Do not retry on auth errors.** If the CLI reports "not signed in" or "access token expired and no refresh token stored", surface the message to the user and ask them to run `inderes login`. Do not attempt to re-authenticate on their behalf.
- **Rate limits** are enforced server-side. Do not loop pagination aggressively across many companies without a specific user request.

## Verification

- `inderes whoami` confirms the user is signed in and shows remaining access-token lifetime.
- `inderes call --list` confirms the CLI can reach the MCP server and shows all 16 tools.
- If a tool returns `isError=true` or an empty `content` array, re-read the arguments — a malformed ID or unknown company silently yields empty responses on some tools.

## Data Scope

Inderes primarily covers Finland and other Nordics (Sweden, Denmark, Norway, Estonia). Some tools also support France, Germany, and USA via the `regions` parameter.

This CLI is an unofficial community wrapper around Inderes's MCP server. It requires the user's own Inderes Premium subscription — the CLI never bypasses auth.
