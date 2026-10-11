// Buddy Bridge — HTTP 代理 + 串口桥 + 网页搜索 + 编程模式
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::{
    body::Body,
    extract::{Query, State as AxumState},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tower_http::cors::{Any, CorsLayer};

#[derive(Parser, Clone)]
#[command(name = "buddy-bridge", version)]
struct Args {
    #[arg(long, default_value = "COM6")]
    serial: String,
    #[arg(long, default_value_t = 115200)]
    baud: u32,
    #[arg(long, default_value = "http://localhost:11434")]
    backend: String,
    #[arg(long, default_value = "127.0.0.1:11500")]
    listen: String,
}

// ─────────────────────────── 状态 ───────────────────────────

struct SharedState {
    running: u32,
    tokens: u64,
    model: String,
    last_done: Option<Instant>,
    log: Vec<String>,
    pending_emote: Option<String>,
}

impl SharedState {
    fn new() -> Self {
        Self {
            running: 0,
            tokens: 0,
            model: String::new(),
            last_done: None,
            log: Vec::new(),
            pending_emote: None,
        }
    }

    fn snapshot_json(&self) -> String {
        let completed = self.last_done
            .map(|t| t.elapsed() < Duration::from_secs(2))
            .unwrap_or(false);
        let msg = if self.running > 0 {
            format!("{} running",
                if self.model.is_empty() { "deepseek" } else { &self.model })
        } else if !self.model.is_empty() {
            format!("{} idle", self.model)
        } else {
            "deepseek idle".to_string()
        };
        let entries: Vec<&String> = self.log.iter().rev().take(8).collect();
        let v = json!({
            "total": 1,
            "running": self.running,
            "waiting": 0,
            "completed": completed,
            "tokens": self.tokens,
            "tokens_today": self.tokens,
            "model": self.model,
            "msg": msg,
            "entries": entries,
        });
        format!("{}\n", v)
    }
}

struct AppState {
    inner: Arc<Mutex<SharedState>>,
    upstream: String,
    client: reqwest::Client,
}

// 内嵌前端
const CHAT_HTML: &str = include_str!("chat.html");

// ─────────────────────────── 网页搜索 ───────────────────────────

async fn web_search(query: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                     AppleWebKit/537.36 (KHTML, like Gecko) \
                     Chrome/120.0.0.0 Safari/537.36")
        .build()?;

    // Bing 搜索
    let url = format!(
        "https://www.bing.com/search?q={}&setlang=zh-CN",
        urlencoding::encode(query)
    );
    println!("[search] GET {}", url);

    let html = client.get(&url).send().await?.text().await?;
    println!("[search] got {} chars", html.len());

    let doc = scraper::Html::parse_document(&html);

    let result_sel = scraper::Selector::parse("li.b_algo").unwrap();
    let title_sel  = scraper::Selector::parse("h2 a").unwrap();
    let snip_sel   = scraper::Selector::parse(".b_caption p").unwrap();

    let mut out = String::new();
    let mut count = 0;

    for res in doc.select(&result_sel) {
        if count >= 5 { break; }

        let title = res.select(&title_sel).next()
            .map(|e| e.text().collect::<String>())
            .unwrap_or_default();
        let href = res.select(&title_sel).next()
            .and_then(|e| e.value().attr("href"))
            .unwrap_or("");
        let snippet = res.select(&snip_sel).next()
            .map(|e| e.text().collect::<String>())
            .unwrap_or_default();

        if title.is_empty() { continue; }

        out.push_str(&format!(
            "[{}. {}]\n{}\n{}\n\n",
            count + 1,
            title.trim(),
            href.trim(),
            snippet.trim()
        ));
        count += 1;
    }

    if out.is_empty() {
        return Ok("（搜索未返回结果）".to_string());
    }
    println!("[search] parsed {} results", count);
    Ok(out)
}
// ─────────────────────────── HTTP handlers ───────────────────────────

async fn index() -> Html<&'static str> {
    Html(CHAT_HTML)
}

async fn health() -> impl IntoResponse {
    Json(json!({"ok": true}))
}

async fn models_proxy(AxumState(state): AxumState<Arc<AppState>>) -> Response {
    let url = format!("{}/v1/models", state.upstream);
    match state.client.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.bytes().await.unwrap_or_default();
            Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body)).unwrap()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("upstream: {}", e)).into_response(),
    }
}

async fn emote_handler(
    AxumState(state): AxumState<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let name = params.get("name").map(|s| s.to_lowercase()).unwrap_or_default();
    const VALID: &[&str] = &["sleep","idle","busy","attention","celebrate","dizzy","heart"];
    if !VALID.contains(&name.as_str()) {
        return Json(json!({"ok": false, "error": format!("unknown emote: {}", name)}));
    }
    {
        let mut s = state.inner.lock().unwrap();
        s.pending_emote = Some(name.clone());
        s.log.push(format!("* emote: {}", name));
        if s.log.len() > 20 { s.log.remove(0); }
    }
    println!("[emote] queued: {}", name);
    Json(json!({"ok": true, "emote": name}))
}

async fn chat_proxy(
    AxumState(state): AxumState<Arc<AppState>>,
    body: String,
) -> Response {
    println!("[chat] incoming POST /v1/chat/completions");

    let mut req_json: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let stream = req_json.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let model = req_json.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let use_search = req_json.get("use_search").and_then(|v| v.as_bool()).unwrap_or(false);
    let mode = req_json.get("mode").and_then(|v| v.as_str()).unwrap_or("chat").to_string();

    // ─── 系统提示词（按模式注入） ───
    let has_system = req_json.get("messages")
        .and_then(|m| m.as_array())
        .map(|arr| arr.iter().any(|m| m.get("role").and_then(|r| r.as_str()) == Some("system")))
        .unwrap_or(false);

    if !has_system {
        let sys_prompt = match mode.as_str() {
            "code" => concat!(
                "你是一个专业的中文编程助手。规则：\n",
                "1. 代码必须放在 ```语言 代码块里，写清语言名\n",
                "2. 优先给可运行的完整代码，不要伪代码\n",
                "3. 指出潜在 bug 和性能问题\n",
                "4. 解释用中文，代码注释用中文\n",
                "5. 除非必要，不要啰嗦\n",
                "6. 不确定的地方直说，不要编造 API\n"
            ),
            "debug" => concat!(
                "你是一个调试助手。用户会给你报错和代码，规则：\n",
                "1. 先分析报错原因（一句话）\n",
                "2. 给出最小修复方案（只改必要的行）\n",
                "3. 用 diff 格式展示改动\n",
                "4. 给一个验证方法\n"
            ),
            "explain" => concat!(
                "你是一个代码讲解助手。规则：\n",
                "1. 先一句话总结代码做什么\n",
                "2. 逐段解释关键逻辑\n",
                "3. 指出设计意图和可能的坑\n",
                "4. 用中文，简洁清楚\n"
            ),
            _ => "你是一个乐于助人的中文助手。回答简洁，重点突出。\n",
        };
        if let Some(arr) = req_json.get_mut("messages").and_then(|m| m.as_array_mut()) {
            arr.insert(0, json!({"role": "system", "content": sys_prompt}));
        }
    }

    // ─── 网页搜索（可选） ───
    if use_search {
        let query = req_json.get("messages")
            .and_then(|m| m.as_array())
            .and_then(|arr| arr.iter().rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user")))
            .and_then(|m| m.get("content"))
            .and_then(|c| {
                if let Some(s) = c.as_str() { Some(s.to_string()) }
                else if let Some(arr) = c.as_array() {
                    arr.iter().find(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
                        .and_then(|p| p.get("text"))
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                } else { None }
            })
            .unwrap_or_default();

        if !query.is_empty() {
            println!("[search] query: {}", query);
            match web_search(&query).await {
                Ok(results) => {
                    println!("[search] got {} chars", results.len());
                    if let Some(arr) = req_json.get_mut("messages").and_then(|m| m.as_array_mut()) {
                        arr.insert(0, json!({
                            "role": "system",
                            "content": format!(
                                "以下是关于「{}」的网络搜索结果。请基于这些信息回答，\
                                 如果信息不足就直说。回答末尾列出引用来源（用链接）。\n\n{}",
                                query, results)
                        }));
                    }
                }
                Err(e) => println!("[search] failed: {}", e),
            }
        }
    }

    let final_body = req_json.to_string();

    // ─── 更新统计 ───
    {
        let mut s = state.inner.lock().unwrap();
        s.running += 1;
        if !model.is_empty() { s.model = model.clone(); }
        let m = format!("> {} running",
            if s.model.is_empty() { "deepseek" } else { &s.model });
        s.log.push(m);
        if s.log.len() > 20 { s.log.remove(0); }
    }

    let url = format!("{}/v1/chat/completions", state.upstream);
    println!("[chat] forwarding to {}", url);
    let start = Instant::now();
    let resp = state.client.post(&url).body(final_body).send().await;

    match resp {
        Ok(up) => {
            let status = up.status();
            println!("[chat] upstream status = {}", status);

            if stream {
                let s_clone = state.clone();
                let chunks = up.bytes_stream();
                let mapped = chunks.map(|c| c.map_err(|e|
                    std::io::Error::new(std::io::ErrorKind::Other, e)));

                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(20)).await;
                    let mut s = s_clone.inner.lock().unwrap();
                    s.running = s.running.saturating_sub(1);
                    s.last_done = Some(Instant::now());
                });

                Response::builder()
                    .status(status)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header(header::CACHE_CONTROL, "no-cache")
                    .body(Body::from_stream(mapped)).unwrap()
            } else {
                let body = up.bytes().await.unwrap_or_default();
                let _ = start;
                {
                    let mut s = state.inner.lock().unwrap();
                    s.running = s.running.saturating_sub(1);
                    s.last_done = Some(Instant::now());
                    if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                        if let Some(n) = v.pointer("/usage/completion_tokens")
                                          .and_then(|x| x.as_u64()) {
                            s.tokens += n;
                        }
                    }
                }
                Response::builder()
                    .status(status)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body)).unwrap()
            }
        }
        Err(e) => {
            println!("[chat] upstream error: {}", e);
            let mut s = state.inner.lock().unwrap();
            s.running = s.running.saturating_sub(1);
            (StatusCode::BAD_GATEWAY, format!("upstream: {}", e)).into_response()
        }
    }
}

async fn run_http(state: Arc<AppState>, listen: String) -> Result<()> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/emote", get(emote_handler))
        .route("/v1/models", get(models_proxy))
        .route("/v1/chat/completions", post(chat_proxy))
        .route("/models", get(models_proxy))
        .route("/chat/completions", post(chat_proxy))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("[http] listening on http://{}", listen);
    println!("[http] routes: /  /health  /emote  /v1/models  /v1/chat/completions");
    axum::serve(listener, app).await?;
    Ok(())
}

// ─────────────────────────── 串口 ───────────────────────────

fn run_serial(port_name: String, baud: u32, state: Arc<AppState>) -> Result<()> {
    let mut port = loop {
        match serialport::new(&port_name, baud)
            .timeout(Duration::from_millis(200))
            .open()
        {
            Ok(p) => break p,
            Err(e) => {
                eprintln!("[serial] open {} failed: {} — retry in 3s", port_name, e);
                thread::sleep(Duration::from_secs(3));
            }
        }
    };
    println!("[serial] opened {} @ {}", port_name, baud);
    thread::sleep(Duration::from_secs(2));

    // 首次时间同步
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let tz_offset: i64 = -8 * 3600;
    let time_msg = format!("{{\"time\":[{},{}]}}\n", now, tz_offset);
    let _ = port.write_all(time_msg.as_bytes());
    let _ = port.flush();

    let mut last_hb = Instant::now() - Duration::from_secs(1);
    loop {
        // 优先发 emote
        let emote = {
            let mut s = state.inner.lock().unwrap();
            s.pending_emote.take()
        };
        if let Some(name) = emote {
            let j = format!("{{\"emote\":\"{}\"}}\n", name);
            println!("[serial] → emote: {}", name);
            if port.write_all(j.as_bytes()).is_err() { break; }
            let _ = port.flush();
            continue;
        }

        // 500ms 心跳
        if last_hb.elapsed() >= Duration::from_millis(500) {
            let snapshot = {
                let s = state.inner.lock().unwrap();
                s.snapshot_json()
            };
            if port.write_all(snapshot.as_bytes()).is_err() { break; }
            let _ = port.flush();
            last_hb = Instant::now();
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

// ─────────────────────────── main ───────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();
    let state = Arc::new(AppState {
        inner: Arc::new(Mutex::new(SharedState::new())),
        upstream: args.backend.trim_end_matches('/').to_string(),
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(900))
            .build()?,
    });

    // HTTP 线程
    {
        let s = state.clone();
        let listen = args.listen.clone();
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all().build().unwrap();
            if let Err(e) = rt.block_on(run_http(s, listen)) {
                eprintln!("[http] {}", e);
            }
        });
    }

    // 串口线程
    {
        let s = state.clone();
        let serial_name = args.serial.clone();
        let baud = args.baud;
        thread::spawn(move || {
            if let Err(e) = run_serial(serial_name, baud, s) {
                eprintln!("[serial] {}", e);
            }
        });
    }

    thread::sleep(Duration::from_millis(800));
    let url = format!("http://{}/", args.listen);
    println!("[main] opening browser at {}", url);
    let _ = open::that(&url);

    println!("[main] running — Ctrl+C to stop");
    loop { thread::sleep(Duration::from_secs(60)); }
}