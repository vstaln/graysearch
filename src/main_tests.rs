use super::*;

#[test]
fn strip_tags_removes_scripts_and_collapses_whitespace() {
    let html = "<p>Hello <b>world</b><script>var x=1;</script></p>  <p>again</p>";
    assert_eq!(strip_tags(html), "Hello world again");
}

#[test]
fn strip_tags_handles_style_block() {
    assert_eq!(strip_tags("<style>.a{color:red}</style><p>x</p>"), "x");
}

#[test]
fn bing_link_passes_through_direct_urls() {
    assert_eq!(bing_link("https://example.com/"), "https://example.com/");
}

#[test]
fn bing_link_decodes_ck_redirect_base64() {
    // u=a1a<base64url(url)>
    let raw = "https://www.bing.com/ck/a?!&p=1&u=a1aHR0cHM6Ly93d3cuc3BlZWR0ZXN0Lm5ldC8&v=2";
    assert_eq!(bing_link(raw), "https://www.speedtest.net/");
}

#[test]
fn bing_link_decodes_with_amp_escapes() {
    let raw = "https://www.bing.com/ck/a?!&amp;&amp;p=1&amp;u=a1aHR0cHM6Ly93d3cuc3BlZWR0ZXN0Lm5ldC8&amp;v=2";
    assert_eq!(bing_link(raw), "https://www.speedtest.net/");
}

#[tokio::test]
async fn search_unknown_pinned_provider_errors_without_network() {
    let http = ua_client().unwrap();
    let err = search(&http, "q", 1, Some("nope")).await.unwrap_err();
    assert_eq!(err, "no provider configured");
}

#[tokio::test]
async fn search_with_no_keys_and_invalid_pinned_falls_through_all() {
    // pinned searxng but no SEARXNG_BASE_URL -> None for all, no network touched
    let http = ua_client().unwrap();
    // SAFETY: single-threaded test process; env not shared with other threads
    // beyond this point. Edition 2024 marks set_var unsafe.
    unsafe { std::env::set_var("SEARXNG_BASE_URL", "") };
    let err = search(&http, "q", 1, Some("searxng")).await.unwrap_err();
    assert_eq!(err, "no provider configured");
}

#[test]
fn relevance_gate_keeps_real_results_and_drops_spam() {
    let q = "droidrun github";
    let real = Result_ {
        title: "droidrun/droidrun: control Android via LLM agents".into(),
        url: "https://github.com/droidrun/droidrun".into(),
        snippet: String::new(),
    };
    let spam = Result_ {
        title: "hot singles in your area".into(),
        url: "https://spam.example/x".into(),
        snippet: String::new(),
    };
    assert!(result_relevant(q, &real));
    assert!(!result_relevant(q, &spam));
}

#[test]
fn relevance_gate_handles_stopword_only_queries() {
    // A query whose tokens are all stopwords falls back to len>=3 tokens —
    // it must not vacuously reject everything.
    let r = Result_ {
        title: "How to use GitHub".into(),
        url: "https://docs.github.com/".into(),
        snippet: String::new(),
    };
    assert!(result_relevant("how to", &r));
}
