# Security policy

## Scope

This policy covers the `inderes-cli` binary, install scripts, and bundled
skill files. It does **not** cover the hosted Inderes MCP server
(`mcp.inderes.com`), the Inderes Keycloak realm, or the Inderes research
data served through them — those belong to Inderes Oyj.

## Supported versions

Only the most recent calendar-versioned release receives security fixes.
Older releases may be safe to keep installed, but fixes will only ever land
on the latest tag; downgrade paths are not maintained.

## Reporting a CLI-side vulnerability

**Please do not open a public GitHub issue.** Use [GitHub's private
vulnerability reporting](https://github.com/heikki-laitala/inderes-cli/security/advisories/new)
for the repository instead. Suitable CLI-side reports include:

- Token exfiltration through the file backend (`~/.config/inderes-cli/tokens.json`).
- Man-in-the-middle attacks against the OAuth flow or MCP client.
- Arbitrary code execution through crafted MCP responses.
- Supply-chain issues in our dependencies (`cargo audit` output).
- Issues in `install.sh` / `install.ps1` (checksum bypass, path traversal, etc.).

Please include: affected version, minimal reproducer, observed vs expected
behaviour, and your disclosure timeline if any.

I'll acknowledge reports within a week, patch in the next calendar-version
release, and credit the reporter unless they prefer anonymity.

## Reporting an Inderes-side issue

Issues with the Inderes MCP server, Keycloak realm, data accuracy, tool
schemas, or the premium subscription go to **[support@inderes.fi](mailto:support@inderes.fi)**,
not here. Include the `inderes-mcp` client name in the subject line so
support knows it's the public API client.
