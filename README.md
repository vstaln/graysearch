<div align="center">
  <h1>gray-search</h1>
  <p><strong>Web search and page fetch for the agent — keyless providers out of the box, API keys upgrade the quality.</strong></p>
  <p>
    <a href="https://gray.alignment.id">Website</a> ·
    <a href="https://github.com/vstaln/graysearch">Source</a> ·
    <a href="https://github.com/vstaln/gray">gray</a>
  </p>
  <p>
    <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-1c1c20?style=flat-square&labelColor=0a0a0b" /></a>
    <a href="https://www.rust-lang.org"><img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-rust-1c1c20?style=flat-square&labelColor=0a0a0b&logo=rust&logoColor=d4a373" /></a>
    <img alt="gray plugin" src="https://img.shields.io/badge/gray-plugin-1c1c20?style=flat-square&labelColor=0a0a0b&color=7aa2f7" />
  </p>
</div>

<br/>
```bash
gray plugin install search
```

`gray-search` gives the agent web search and page fetches over the
sidecar tool protocol. Search answers without any keys — tavily's
keyless mode, exa's public MCP endpoint, DuckDuckGo, Bing and GitHub's
repo-search API are all tried — and optional keys or a self-hosted
SearXNG move earlier in the provider chain for better results.

## Tools

| Tool | What it does |
|---|---|
| `web_search` | Search the web. Provider chain: pinned > searxng > tavily > brave > exa > duckduckgo > bing > github. Params: `query` (required), `provider`, `max_results`. |
| `web_fetch` | Fetch a URL and return readable page text. |

## Commands

| Command | What it does |
|---|---|
| `/search <query>` | Run a web search from the prompt line. |

## Configuration

No configuration is required — the keyless providers work as installed.
To raise quality set any of:

| Env var | Effect |
|---|---|
| `SEARXNG_BASE_URL` | Self-hosted SearXNG instance; becomes the preferred provider |
| `TAVILY_API_KEY` | Upgrades tavily from rate-limited keyless mode to Bearer auth |
| `BRAVE_API_KEY` | Enables Brave web search |
| `EXA_API_KEY` | Authenticated exa instead of the public MCP endpoint |

`HTTPS_PROXY`/`http_proxy` are honored for all provider traffic.
