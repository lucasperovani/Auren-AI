# Web Research

Search the web and read pages as clean text, without any API key.

## What it does

| Tool | Purpose |
|---|---|
| `web_search` | Search the web; returns title, URL, and snippet per result |
| `web_fetch` | Open a public page and return its readable text and title |
| `web_navigate` | Render a page in a headless browser, for JavaScript-heavy sites |

## Which tool to use

1. `web_search` when you need to find something.
2. `web_fetch` on the most promising result — fast, no browser needed.
3. `web_navigate` only when `web_fetch` comes back nearly empty, which means
   the page builds itself with JavaScript (SPAs, dashboards, infinite scroll).

`web_navigate` is slower (a browser starts per call) and needs Chromium on the
server. If it is not installed, the tool reports that clearly instead of
failing silently.

## How search works

`web_search` uses DuckDuckGo's public HTML endpoint (`html.duckduckgo.com`).
No credentials, no account, no rate-limit key. Results are parsed from the
returned markup, and DuckDuckGo's redirect wrapper (`/l/?uddg=…`) is unwrapped
so you get the real destination URL.

If a deployment later configures a keyed search provider, that can be layered
behind the same tool name without changing the agent's behavior.

## Why text instead of HTML

`web_fetch` converts the page to text before returning it:

- drops `script`, `style`, `noscript`, `svg`, `iframe`, `head`, `canvas`
- turns block elements into line breaks and collapses whitespace
- decodes HTML entities (`&amp;`, `&#8212;`, `&nbsp;`)
- caps the result at 40,000 characters and reports `truncated`

Returning raw HTML burns most of the model's context on markup it cannot use.
A typical page shrinks by roughly 70–90% while keeping the prose.

## Safety

Retrieved content is **untrusted input**. The tools never execute anything from
a page and never follow instructions found in it.

Network access is restricted:

- only `http`/`https` URLs
- loopback, private, link-local, CGNAT, and multicast/broadcast addresses are
  refused
- `localhost`, `*.internal`, `*.local`, `*.home.arpa`, and `metadata` hosts are
  refused by name
- **every redirect hop is re-validated**, and the chain is capped at 5 hops.
  This closes the case where a public URL replies `302 → 169.254.169.254` and
  the client would otherwise follow it into the cloud metadata service.
- bodies are capped at 2 MB and requests time out at 20 seconds

No polling loops exist: a fetch is one bounded request, and the agent decides
whether to try another URL.

## Limitations

- `web_fetch` cannot see JavaScript-rendered content; use `web_navigate`.
- Sites behind a login or paywall are not accessible.
- DuckDuckGo occasionally serves a challenge page; when that happens the search
  returns no results rather than failing noisily.

## Headless browser

`web_navigate` runs `chromium --headless --dump-dom`: it navigates, lets
scripts run for a virtual-time budget, prints the rendered DOM, and exits. There
is no daemon to supervise and no browser state shared between calls.

Browser discovery order:

1. `AUREN_BROWSER_PATH` (absolute path to any Chromium-compatible binary)
2. `chromium`, `chromium-browser`, `google-chrome`, `google-chrome-stable`,
   `chrome`, `msedge`, `headless_shell`, `brave-browser` in `PATH`

The Docker image installs Alpine's `chromium` and sets `AUREN_BROWSER_PATH`. On
a desktop install the tool is available only if a browser is present; otherwise
it reports that no browser is installed.

Chromium's own sandbox is disabled only inside a container (detected via
`/.dockerenv`), where the kernel sandbox is usually unavailable.

