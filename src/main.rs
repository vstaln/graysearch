//! gray-search: web search sidecar (protocol v1.1).
//! Tools: web_search, web_fetch. Command: /search <query>.
//! Provider chain: pinned > searxng > tavily > brave > exa > duckduckgo > bing > github.
//! Keyless tier: tavily (X-Tavily-Access-Mode: keyless) and exa
//! (mcp.exa.ai) are real APIs that answer without keys — the same
//! providers opencode ships. Scrapers (duckduckgo html/lite, bing) sit
//! below them: bing serves spam/poisoned SERPs to scrape UAs, so its
//! results must pass a relevance gate and any failure falls through
//! rather than surfacing poison. GitHub's
//! repo-search JSON API is keyless and sits last: wrong shape for
//! non-dev questions, but it stays reachable where SERPs are blocked.

use std::io::Write;

use base64::Engine;
use regex::Regex;
use serde_json::{Value, json};

#[derive(serde::Deserialize)]
struct Request {
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

struct Result_ {
    title: String,
    url: String,
    snippet: String,
}

/// A real desktop-Chrome UA + browser headers: a self-identifying bot UA
/// is what buckets the client into "scrape traffic" (poisoned/degraded
/// SERPs), and reqwest already picks up HTTPS_PROXY/http_proxy env vars
/// automatically, so a clean egress proxy fixes every provider at once.
fn ua_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .default_headers({
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9".parse().unwrap());
            h
        })
        .timeout(std::time::Duration::from_secs(20))
        .build()?)
}

/// The plugin manifest, identical on the wire (`plugin/manifest` result)
/// and on the CLI (`gray-search manifest`, the host's registration probe).
/// Version tracks Cargo.toml via `env!` so the two can never drift.
fn manifest_json() -> Value {
    json!({
        "name": "search", "version": env!("CARGO_PKG_VERSION"), "protocol": "1.1",
        "tools": [
            {"name": "web_search", "description": "Search the web (keyless: tavily, exa, duckduckgo, bing, github; better with SEARXNG_BASE_URL, TAVILY_API_KEY, BRAVE_API_KEY, or EXA_API_KEY set)", "parameters": {"type": "object", "properties": {"query": {"type": "string"}, "max_results": {"type": "integer"}, "provider": {"type": "string"}}, "required": ["query"]}, "snippet": "web_search <query>"},
            {"name": "web_fetch", "description": "Fetch a URL and return its text content", "parameters": {"type": "object", "properties": {"url": {"type": "string"}, "max_chars": {"type": "integer"}}, "required": ["url"]}, "snippet": "web_fetch <url>"}
        ],
        "commands": ["/search"], "hooks": [],
    })
}

/// Shared HTTP client, built lazily on first use. `reqwest::Client::build`
/// is fallible (TLS backend init, resolver setup); a failure must never
/// reach startup — before the manifest handshake the host reads ANY exit
/// as "sidecar failed to spawn". The cell retries on the next request and
/// a build failure surfaces as a tool error, not a spawn failure.
type HttpCell = tokio::sync::OnceCell<reqwest::Client>;

async fn http_client(cell: &HttpCell) -> anyhow::Result<&reqwest::Client> {
    cell.get_or_try_init(|| std::future::ready(ua_client())).await
}

fn strip_tags(html: &str) -> String {
    let mut no_js = html.to_string();
    for tag in ["script", "style"] {
        let re = Regex::new(&format!(r"(?is)<{tag}[^>]*>.*?</{tag}>")).unwrap();
        no_js = re.replace_all(&no_js, " ").into();
    }
    let no_js = no_js;
    let re = Regex::new(r"(?s)<[^>]+>").unwrap();
    let no_tags = re.replace_all(&no_js, " ");
    Regex::new(r"\s+").unwrap().replace_all(&no_tags, " ").trim().to_string()
}

fn to_json(results: Vec<Result_>) -> Vec<Value> {
    results.into_iter().map(|r| json!({"title": r.title, "url": r.url, "snippet": r.snippet})).collect()
}

async fn searxng(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    let base = std::env::var("SEARXNG_BASE_URL").unwrap_or_default();
    if base.is_empty() { return Ok(None); }
    let url = format!("{}/search?q={}&format=json", base.trim_end_matches('/'), url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>());
    let d: Value = http.get(&url).send().await?.json().await?;
    Ok(Some(d["results"].as_array().unwrap_or(&vec![]).iter().take(n).map(|r| Result_ {
        title: r["title"].as_str().unwrap_or("").into(),
        url: r["url"].as_str().unwrap_or("").into(),
        snippet: r["content"].as_str().unwrap_or("").into(),
    }).collect()))
}

/// Tavily — a real search API that also answers with NO key: the
/// `X-Tavily-Access-Mode: keyless` header (the mode opencode uses by
/// default) gets rate-limited keyless access. A configured
/// TAVILY_API_KEY upgrades to Bearer auth.
async fn tavily(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    let key = std::env::var("TAVILY_API_KEY").unwrap_or_default();
    let req = http
        .post("https://api.tavily.com/search")
        .json(&json!({
            "query": q,
            "search_depth": "basic",
            "chunks_per_source": 3,
            "max_results": n,
        }));
    let req = if key.is_empty() {
        req.header("X-Tavily-Access-Mode", "keyless")
    } else {
        req.bearer_auth(key)
    };
    let d: Value = req.send().await?.error_for_status()?.json().await?;
    Ok(Some(d["results"].as_array().unwrap_or(&vec![]).iter().take(n).map(|r| Result_ {
        title: r["title"].as_str().unwrap_or("").into(),
        url: r["url"].as_str().unwrap_or("").into(),
        snippet: r["content"].as_str().unwrap_or("").into(),
    }).collect()))
}

async fn brave(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    let key = std::env::var("BRAVE_API_KEY").unwrap_or_default();
    if key.is_empty() { return Ok(None); }
    let d: Value = http.get(format!("https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>(), n))
        .header("X-Subscription-Token", key).header("Accept", "application/json")
        .send().await?.json().await?;
    Ok(Some(d["web"]["results"].as_array().unwrap_or(&vec![]).iter().take(n).map(|r| Result_ {
        title: r["title"].as_str().unwrap_or("").into(),
        url: r["url"].as_str().unwrap_or("").into(),
        snippet: r["description"].as_str().unwrap_or("").into(),
    }).collect()))
}

/// Exa — keyless via their public MCP endpoint `mcp.exa.ai/mcp` (the same
/// path opencode uses): a JSON-RPC `tools/call` on `web_search_exa`
/// answers over streamable HTTP without an initialize handshake.
/// EXA_API_KEY is appended as `?exaApiKey=` when configured.
async fn exa(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    let key = std::env::var("EXA_API_KEY").unwrap_or_default();
    let endpoint = if key.is_empty() {
        "https://mcp.exa.ai/mcp".to_string()
    } else {
        format!("https://mcp.exa.ai/mcp?exaApiKey={key}")
    };
    let body = http
        .post(endpoint)
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "web_search_exa", "arguments": {"query": q, "numResults": n}},
        }))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    // Streamable-HTTP SSE: results ride the last `data:` line's
    // result.content[].text as "---"-separated blocks.
    let mut text = String::new();
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                if let Some(t) = v["result"]["content"].as_array().and_then(|c| {
                    c.iter().find_map(|i| i["text"].as_str())
                }) {
                    text = t.to_string();
                }
            }
        }
    }
    if text.is_empty() {
        // Some deployments answer plain JSON-RPC instead of SSE.
        if let Ok(v) = serde_json::from_str::<Value>(&body) {
            if let Some(t) = v["result"]["content"].as_array().and_then(|c| {
                c.iter().find_map(|i| i["text"].as_str())
            }) {
                text = t.to_string();
            }
        }
    }
    Ok(Some(parse_exa_blocks(&text)))
}

/// Split Exa's "Title:/URL:/Highlights:" text blocks into results — a
/// port of opencode's parseResults.
fn parse_exa_blocks(text: &str) -> Vec<Result_> {
    text.split("\n\n---\n\n")
        .filter_map(|block| {
            let url = block
                .lines()
                .find_map(|l| l.strip_prefix("URL:").map(str::trim))?;
            let title = block
                .lines()
                .find_map(|l| l.strip_prefix("Title:").map(str::trim))
                .unwrap_or_default();
            let snippet = block
                .split_once("Highlights:")
                .or_else(|| block.split_once("Text:"))
                .map(|(_, t)| t.trim().chars().take(500).collect())
                .unwrap_or_default();
            Some(Result_ {
                title: title.to_string(),
                url: url.to_string(),
                snippet,
            })
        })
        .collect()
}

fn bing_link(raw: &str) -> String {
    if raw.contains("bing.com/ck/a") {
        if let Ok(u) = url::Url::parse(&raw.replace("&amp;", "&")) {
            for (k, v) in u.query_pairs() {
                if k == "u" && v.len() > 2 {
                    let stripped = &v[2..];
                    let padded = format!("{stripped}{}", "=".repeat((4 - stripped.len() % 4) % 4));
                    if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(stripped)
                        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(&padded))
                    {
                        if let Ok(s) = String::from_utf8(bytes) { return s; }
                    }
                }
            }
        }
    }
    raw.to_string()
}

async fn bing(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    // adlt=strict + cc=US: force SafeSearch on and pin the market — the
    // geo/low-trust tier is where the porn-spam SERPs come from.
    let url = format!("https://www.bing.com/search?q={}&count={}&mkt=en-US&setlang=en&cc=US&adlt=strict",
        url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>(), n);
    let html = http.get(&url).send().await?.text().await?;
    let re = Regex::new(r#"(?s)<h2[^>]*><a[^>]*href="([^"]+)"[^>]*>(.*?)</a></h2>"#)?;
    let mut out = vec![];
    for c in re.captures_iter(&html) {
        let title = strip_tags(&c[2]);
        let end = c.get(0).unwrap().end();
        let mut stop = (end + 2000).min(html.len());
        while !html.is_char_boundary(stop) {
            stop -= 1;
        }
        let window = &html[end..stop];
        let snippet = Regex::new(r"(?s)<p[^>]*>(.*?)</p>")?.captures(window)
            .and_then(|p| p.get(1)).map(|p| strip_tags(p.as_str())).unwrap_or_default();
        out.push(Result_ { title, url: bing_link(&c[1]), snippet });
        if out.len() >= n { break; }
    }
    // Bing serves spam/poisoned SERPs to scrape UAs (tech queries have
    // returned literal porn listings). A poisoned page shares no vocabulary
    // with the query, so drop results without token overlap; an emptied
    // list becomes None and the chain falls through instead of surfacing
    // poison.
    let total = out.len();
    let kept: Vec<_> = out.into_iter().filter(|r| result_relevant(q, r)).collect();
    if kept.is_empty() && total > 0 {
        // Provider answered with results that share no vocabulary with the
        // query — a poisoned SERP, surfaced as an error (unlike an empty
        // page, which quietly falls through as unconfigured).
        return Err(anyhow::anyhow!(
            "{total} results failed the relevance check — likely a poisoned SERP"
        ));
    }
    Ok(if kept.is_empty() { None } else { Some(kept) })
}

/// Query-word overlap check: a result counts as real only when some
/// meaningful query token appears in its title or URL. Bing's poisoned
/// SERPs fail this outright; one-off synonyms can still drop a legit row,
/// but the poison case (zero overlap anywhere) is what matters.
fn result_relevant(query: &str, r: &Result_) -> bool {
    const STOP: [&str; 16] = [
        "the", "and", "for", "how", "what", "with", "you", "are", "not",
        "all", "can", "this", "that", "from", "your", "use",
    ];
    let words: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && !STOP.contains(w))
        .map(String::from)
        .collect();
    let hay = format!("{} {}", r.title.to_lowercase(), r.url.to_lowercase());
    let words = if words.is_empty() {
        query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 3)
            .map(String::from)
            .collect()
    } else {
        words
    };
    words.is_empty() || words.iter().any(|w| hay.contains(w.as_str()))
}

async fn duckduckgo_html(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    let url = format!("https://html.duckduckgo.com/html/?q={}",
        url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>());
    let html = http.get(&url).send().await?.text().await?;
    let re = Regex::new(r#"(?s)class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)?;
    let mut out = vec![];
    for c in re.captures_iter(&html) {
        out.push(Result_ { title: strip_tags(&c[2]), url: ddg_link(&c[1]), snippet: String::new() });
        if out.len() >= n { break; }
    }
    Ok(if out.is_empty() { None } else { Some(out) })
}

/// The DDG redirect (`/l/?uddg=…`) both endpoints wrap result URLs in.
fn ddg_link(raw: &str) -> String {
    if raw.starts_with("//duckduckgo.com/l/")
        && let Ok(u) = url::Url::parse(&format!("https:{raw}"))
    {
        for (k, v) in u.query_pairs() {
            if k == "uddg" {
                return v.to_string();
            }
        }
    }
    raw.to_string()
}

/// `lite.duckduckgo.com` — a POST form and a bare table; the most
/// scrape-friendly DDG surface. Runs when the html endpoint answers an
/// empty/blocked page.
async fn duckduckgo_lite(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    let html = http
        .post("https://lite.duckduckgo.com/lite/")
        .form(&[("q", q)])
        .send()
        .await?
        .text()
        .await?;
    let re = Regex::new(r#"(?s)<a[^>]*class="result-link"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)?;
    let mut out = vec![];
    for c in re.captures_iter(&html) {
        out.push(Result_ {
            title: strip_tags(&c[2]),
            url: ddg_link(&c[1]),
            snippet: String::new(),
        });
        if out.len() >= n { break; }
    }
    Ok(if out.is_empty() { None } else { Some(out) })
}

async fn duckduckgo(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    match duckduckgo_html(http, q, n).await {
        Ok(Some(r)) => Ok(Some(r)),
        Ok(None) => duckduckgo_lite(http, q, n).await,
        Err(e) => {
            // A dead html endpoint should not mask a working lite one.
            match duckduckgo_lite(http, q, n).await {
                Ok(r) => Ok(r),
                Err(e2) => Err(e2.context(format!("html endpoint also failed: {e:#}"))),
            }
        }
    }
}

/// GitHub repository search — keyless JSON API (10 req/min unauth), and
/// api.github.com stays reachable where every HTML SERP is blocked or
/// serving shells. Last in the chain on purpose: repo-shaped answers are
/// the wrong shape for non-dev questions, but for "X github" queries
/// they're better than a dead end. Same relevance gate applies.
async fn github(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    // Repo search ANDs every term, so a full sentence ("bytedance UI-TARS
    // github GUI agent model") matches nothing. Relax progressively:
    // full query -> meta-words ("github"/"repo") dropped -> top two tokens.
    let mut tried = std::collections::HashSet::new();
    for variant in github_query_variants(q) {
        if !tried.insert(variant.clone()) { continue; }
        let d: Value = http
            .get(format!(
                "https://api.github.com/search/repositories?q={}&per_page={}",
                url::form_urlencoded::byte_serialize(variant.as_bytes()).collect::<String>(),
                n
            ))
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut out = vec![];
        for it in d["items"].as_array().into_iter().flatten() {
            let url = it["html_url"].as_str().unwrap_or_default();
            if url.is_empty() { continue; }
            let name = it["full_name"].as_str().unwrap_or_default();
            let desc = it["description"].as_str().unwrap_or_default();
            let stars = it["stargazers_count"].as_u64().unwrap_or(0);
            out.push(Result_ {
                title: name.to_string(),
                url: url.to_string(),
                snippet: format!("\u{2605}{stars} \u{2014} {desc}"),
            });
            if out.len() >= n { break; }
        }
        let kept: Vec<_> = out.into_iter().filter(|r| result_relevant(q, r)).collect();
        if !kept.is_empty() { return Ok(Some(kept)); }
    }
    Ok(None)
}

/// Query relaxations for the AND'd repo search: verbatim, then with
/// meta-words stripped, then the two leading distinctive tokens.
fn github_query_variants(q: &str) -> Vec<String> {
    const META: [&str; 3] = ["github", "repo", "repository"];
    let meaningful: Vec<&str> = q
        .split_whitespace()
        .filter(|w| !META.contains(&w.to_lowercase().as_str()))
        .collect();
    let mut v = vec![q.to_string(), meaningful.join(" ")];
    if meaningful.len() > 2 {
        v.push(meaningful[..2].join(" "));
    }
    v.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

type Provider<'a> = fn(&'a reqwest::Client, &'a str, usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Option<Vec<Result_>>>> + Send + 'a>>;

async fn search(http: &reqwest::Client, query: &str, n: usize, pinned: Option<&str>) -> Result<(String, Vec<Value>), String> {
    let providers: [(&str, Provider<'_>); 7] = [
        ("searxng", |h, q, n| Box::pin(searxng(h, q, n))),
        ("tavily", |h, q, n| Box::pin(tavily(h, q, n))),
        ("brave", |h, q, n| Box::pin(brave(h, q, n))),
        ("exa", |h, q, n| Box::pin(exa(h, q, n))),
        ("duckduckgo", |h, q, n| Box::pin(duckduckgo(h, q, n))),
        ("bing", |h, q, n| Box::pin(bing(h, q, n))),
        ("github", |h, q, n| Box::pin(github(h, q, n))),
    ];
    let mut errors = vec![];
    for (name, f) in providers {
        if let Some(p) = pinned { if p != name { continue; } }
        match f(http, query, n).await {
            Ok(Some(r)) if !r.is_empty() => return Ok((name.to_string(), to_json(r))),
            Ok(_) => {}
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    Err(if errors.is_empty() { "no provider configured".into() } else { errors.join("; ") })
}

fn main() {
    // Registration probe: `gray-search manifest` prints the manifest JSON
    // and exits 0. It must run ahead of every fallible init — the host
    // invokes it before writing a single frame, and a silent exit there is
    // reported as a spawn failure.
    if std::env::args().nth(1).as_deref() == Some("manifest") {
        println!("{}", manifest_json());
        return;
    }
    if let Err(e) = run() {
        // stdout carries protocol frames only; diagnostics go to stderr.
        eprintln!("gray-search: fatal: {e:#}");
        std::process::exit(1);
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn run() -> anyhow::Result<()> {
    let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(tokio::io::stdin()));
    // Nothing fallible may run between here and the read loop: the host
    // treats a pre-handshake exit as a spawn failure. The HTTP client is
    // built on first tool use instead.
    let http = HttpCell::new();
    let mut stdout = std::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() { continue; }
        let request: Request = match serde_json::from_str(&line) { Ok(r) => r, Err(_) => continue };
        if request.method == "plugin/shutdown" { break; }
        if request.id.is_null() { continue; }
        let params = request.params.clone().unwrap_or_else(|| json!({}));
        let reply: anyhow::Result<Value> = match request.method.as_str() {
            "plugin/manifest" => Ok(manifest_json()),
            "command/run" => {
                let argv: Vec<String> = params.get("argv").and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
                match http_client(&http).await {
                    Err(e) => Ok(json!({"text": format!("search failed: {e:#}")})),
                    Ok(http) => match search(http, &argv.join(" "), 8, None).await {
                        Ok((name, results)) => Ok(json!({"text": format!("[{name}] {}", serde_json::to_string_pretty(&results).unwrap_or_default()).chars().take(4000).collect::<String>()})),
                        Err(e) => Ok(json!({"text": format!("search failed: {e}")})),
                    },
                }
            }
            "tool/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("args").cloned().unwrap_or(json!({}));
                match name {
                    "web_search" => match http_client(&http).await {
                        Err(e) => Ok(json!({"content": format!("error: {e:#}"), "is_error": true})),
                        Ok(http) => {
                            let q = args.get("query").and_then(Value::as_str).unwrap_or("");
                            let n = args.get("max_results").and_then(Value::as_u64).unwrap_or(8) as usize;
                            let pinned = args.get("provider").and_then(Value::as_str);
                            match search(http, q, n, pinned).await {
                                Ok((pname, results)) => Ok(json!({"content": serde_json::to_string_pretty(&json!({"provider": pname, "results": results})).unwrap_or_default().chars().take(8000).collect::<String>()})),
                                Err(e) => Ok(json!({"content": format!("error: {e}"), "is_error": true})),
                            }
                        }
                    },
                    "web_fetch" => match http_client(&http).await {
                        Err(e) => Ok(json!({"content": format!("error: {e:#}"), "is_error": true})),
                        Ok(http) => {
                            let url = args.get("url").and_then(Value::as_str).unwrap_or("");
                            let max = args.get("max_chars").and_then(Value::as_u64).unwrap_or(8000) as usize;
                            match http.get(url).send().await {
                                Ok(resp) => {
                                    let status = resp.status();
                                    match resp.text().await {
                                        Ok(html) if status.is_success() => Ok(json!({"content": strip_tags(&html).chars().take(max).collect::<String>()})),
                                        Ok(_) => Ok(json!({"content": format!("error: HTTP {status} fetching {url}"), "is_error": true})),
                                        Err(e) => Ok(json!({"content": format!("error: {e}"), "is_error": true})),
                                    }
                                }
                                Err(e) => Ok(json!({"content": format!("error: {e}"), "is_error": true})),
                            }
                        }
                    },
                    _ => Ok(json!({"content": format!("error: unknown tool {name}"), "is_error": true})),
                }
            }
            other => {
                let r = json!({"id": request.id, "error": {"code": -32601,
                    "message": format!("method not found: {other}")}});
                writeln!(stdout, "{r}")?;
                stdout.flush()?;
                continue;
            }
        };
        let response = match reply {
            Ok(result) => json!({"id": request.id, "result": result}),
            Err(e) => json!({"id": request.id, "error": {"message": e.to_string()}}),
        };
        writeln!(stdout, "{}", response)?;
        stdout.flush()?;
    }
    Ok(())
}

#[path = "main_tests.rs"]
#[cfg(test)]
mod tests;
