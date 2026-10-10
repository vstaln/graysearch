//! gray-search: web search sidecar (protocol v1.1).
//! Tools: web_search, web_fetch. Command: /search <query>.
//! Provider chain: pinned > searxng > tavily > brave > exa > bing > duckduckgo.

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

fn ua_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (gray-search/0.1)")
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
            {"name": "web_search", "description": "Search the web", "parameters": {"type": "object", "properties": {"query": {"type": "string"}, "max_results": {"type": "integer"}, "provider": {"type": "string"}}, "required": ["query"]}, "snippet": "web_search <query>"},
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

async fn tavily(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    let key = std::env::var("TAVILY_API_KEY").unwrap_or_default();
    if key.is_empty() { return Ok(None); }
    let d: Value = http.post("https://api.tavily.com/search")
        .json(&json!({"api_key": key, "query": q, "max_results": n})).send().await?.json().await?;
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

async fn exa(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    let key = std::env::var("EXA_API_KEY").unwrap_or_default();
    if key.is_empty() { return Ok(None); }
    let d: Value = http.post("https://api.exa.ai/search").header("x-api-key", key)
        .json(&json!({"query": q, "numResults": n})).send().await?.json().await?;
    Ok(Some(d["results"].as_array().unwrap_or(&vec![]).iter().take(n).map(|r| Result_ {
        title: r["title"].as_str().unwrap_or("").into(),
        url: r["url"].as_str().unwrap_or("").into(),
        snippet: r["text"].as_str().unwrap_or("").into(),
    }).collect()))
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
    let url = format!("https://www.bing.com/search?q={}&count={}&mkt=en-US&setlang=en",
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
    Ok(if out.is_empty() { None } else { Some(out) })
}

async fn duckduckgo(http: &reqwest::Client, q: &str, n: usize) -> anyhow::Result<Option<Vec<Result_>>> {
    let url = format!("https://html.duckduckgo.com/html/?q={}",
        url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>());
    let html = http.get(&url).send().await?.text().await?;
    let re = Regex::new(r#"(?s)class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)?;
    let mut out = vec![];
    for c in re.captures_iter(&html) {
        let mut link = c[1].to_string();
        if link.starts_with("//duckduckgo.com/l/") {
            if let Ok(u) = url::Url::parse(&format!("https:{link}")) {
                for (k, v) in u.query_pairs() { if k == "uddg" { link = v.to_string(); } }
            }
        }
        out.push(Result_ { title: strip_tags(&c[2]), url: link, snippet: String::new() });
        if out.len() >= n { break; }
    }
    Ok(if out.is_empty() { None } else { Some(out) })
}

type Provider<'a> = fn(&'a reqwest::Client, &'a str, usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Option<Vec<Result_>>>> + Send + 'a>>;

async fn search(http: &reqwest::Client, query: &str, n: usize, pinned: Option<&str>) -> Result<(String, Vec<Value>), String> {
    let providers: [(&str, Provider<'_>); 6] = [
        ("searxng", |h, q, n| Box::pin(searxng(h, q, n))),
        ("tavily", |h, q, n| Box::pin(tavily(h, q, n))),
        ("brave", |h, q, n| Box::pin(brave(h, q, n))),
        ("exa", |h, q, n| Box::pin(exa(h, q, n))),
        ("bing", |h, q, n| Box::pin(bing(h, q, n))),
        ("duckduckgo", |h, q, n| Box::pin(duckduckgo(h, q, n))),
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
                                Ok(resp) => match resp.text().await {
                                    Ok(html) => Ok(json!({"content": strip_tags(&html).chars().take(max).collect::<String>()})),
                                    Err(e) => Ok(json!({"content": format!("error: {e}"), "is_error": true})),
                                },
                                Err(e) => Ok(json!({"content": format!("error: {e}"), "is_error": true})),
                            }
                        }
                    },
                    _ => Ok(json!({"content": format!("error: unknown tool {name}"), "is_error": true})),
                }
            }
            _ => Ok(json!({})),
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
