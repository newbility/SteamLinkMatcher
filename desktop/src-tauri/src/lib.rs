use anyhow::{anyhow, Context};
use encoding_rs::GBK;
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::Proxy;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;
use zip::ZipArchive;

const DEFAULT_REQUEST_DELAY_MS: u64 = 1500;
const USER_AGENT: &str = "SteamLinkMatcher/0.2 (+tauri)";
const SEARCH_CACHE_VERSION: &str = "v2";

#[derive(Debug, Clone)]
struct AppState {
    data_dir: PathBuf,
    last_steam_request_at: Arc<Mutex<Option<Instant>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    app_id: String,
    title: String,
    url: String,
    score: i32,
    confidence: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MatchResult {
    original: String,
    cleaned: String,
    status: String,
    steam_title: String,
    app_id: String,
    url: String,
    confidence: String,
    score: i32,
    needs_review: bool,
    source: String,
    candidates: Vec<Candidate>,
    message: String,
    from_cache: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    proxy_url: String,
    request_delay_ms: u64,
    data_dir: String,
    portable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveSettingsRequest {
    proxy_url: String,
    request_delay_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxyTestRequest {
    proxy_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyTestResult {
    ok: bool,
    message: String,
}

#[derive(Debug, Deserialize)]
struct MatchRequest {
    name: String,
    force: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualRequest {
    original: String,
    url: String,
    steam_title: Option<String>,
    candidates: Option<Vec<Candidate>>,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClearCacheRequest {
    mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportRequest {
    path: String,
    kind: String,
    results: Vec<MatchResult>,
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Result<Settings, String> {
    let proxy_url = get_setting(&state, "proxy_url", "")?;
    let request_delay_ms = get_request_delay_ms(&state)?;
    Ok(Settings {
        proxy_url,
        request_delay_ms,
        data_dir: state.data_dir.to_string_lossy().to_string(),
        portable: is_portable_dir(&state.data_dir),
    })
}

#[tauri::command]
fn save_settings(req: SaveSettingsRequest, state: State<AppState>) -> Result<Settings, String> {
    validate_proxy_url(&req.proxy_url)?;
    set_setting(&state, "proxy_url", &req.proxy_url)?;
    let delay = req.request_delay_ms.clamp(0, 10_000);
    set_setting(&state, "request_delay_ms", &delay.to_string())?;
    get_settings(state)
}

#[tauri::command]
fn test_proxy(req: ProxyTestRequest, state: State<AppState>) -> Result<ProxyTestResult, String> {
    validate_proxy_url(&req.proxy_url)?;
    let url =
        "https://store.steampowered.com/search/?term=Corpse%20Keeper&category1=998&ndl=1&l=english";
    let page =
        fetch_url_with_proxy(&state, url, 12, req.proxy_url.trim()).map_err(display_error)?;
    let ok = page.contains("Corpse Keeper") || page.contains("search_results");
    Ok(ProxyTestResult {
        ok,
        message: if ok {
            if req.proxy_url.trim().is_empty() {
                "Steam 连接测试成功".to_string()
            } else {
                "代理测试成功".to_string()
            }
        } else {
            "已连接，但返回内容不像 Steam 搜索页".to_string()
        },
    })
}

#[tauri::command]
async fn match_game(req: MatchRequest, state: State<'_, AppState>) -> Result<MatchResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        match_game_inner(&req.name, req.force.unwrap_or(false), &state).map_err(display_error)
    })
    .await
    .map_err(display_error)?
}

#[tauri::command]
fn manual_update(req: ManualRequest, state: State<AppState>) -> Result<MatchResult, String> {
    manual_update_inner(req, &state).map_err(display_error)
}

#[tauri::command]
fn confirm_result(mut result: MatchResult, state: State<AppState>) -> Result<MatchResult, String> {
    if result.original.trim().is_empty() || result.url.trim().is_empty() {
        return Err("只能确认已有链接的记录".to_string());
    }
    result.status = "已匹配".to_string();
    result.needs_review = false;
    result.source = "人工确认".to_string();
    result.message.clear();
    result.from_cache = false;
    save_match(&state, &result.original, &result.cleaned, &result, "manual")?;
    Ok(result)
}

#[tauri::command]
fn clear_cache(req: ClearCacheRequest, state: State<AppState>) -> Result<(), String> {
    let conn = db(&state).map_err(display_error)?;
    conn.execute("DELETE FROM search_cache", [])
        .map_err(display_error)?;
    if req.mode == "all" {
        conn.execute("DELETE FROM matches", [])
            .map_err(display_error)?;
    } else if req.mode != "search" {
        return Err("缓存清理模式只能是 all 或 search".to_string());
    }
    Ok(())
}

#[tauri::command]
fn import_file(path: String) -> Result<Vec<String>, String> {
    let path = PathBuf::from(path);
    let bytes = fs::read(&path).map_err(display_error)?;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let names = match ext.as_str() {
        "csv" => parse_csv(&bytes).map_err(display_error)?,
        "xlsx" => parse_xlsx(&bytes).map_err(display_error)?,
        _ => parse_txt(&bytes),
    };
    Ok(drop_known_header(names))
}

#[tauri::command]
fn export_file(req: ExportRequest) -> Result<(), String> {
    let path = PathBuf::from(req.path);
    if req.kind == "xlsx" {
        export_xlsx(&path, &req.results).map_err(display_error)
    } else {
        export_csv(&path, &req.results).map_err(display_error)
    }
}

#[tauri::command]
fn open_external_url(url: String, app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(display_error)
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("cache.sqlite3")
}

fn db(state: &AppState) -> anyhow::Result<Connection> {
    fs::create_dir_all(&state.data_dir)?;
    let conn = Connection::open(db_path(&state.data_dir))?;
    init_db(&conn)?;
    Ok(conn)
}

fn init_db(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS matches (
          cache_key TEXT PRIMARY KEY,
          original TEXT NOT NULL,
          cleaned TEXT NOT NULL,
          result_json TEXT NOT NULL,
          source TEXT NOT NULL,
          updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS search_cache (
          query_key TEXT PRIMARY KEY,
          query TEXT NOT NULL,
          candidates_json TEXT NOT NULL,
          updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS settings (
          key TEXT PRIMARY KEY,
          value TEXT NOT NULL,
          updated_at INTEGER NOT NULL
        );
        "#,
    )?;
    Ok(())
}

fn get_setting(state: &AppState, key: &str, default: &str) -> Result<String, String> {
    let conn = db(state).map_err(display_error)?;
    let mut stmt = conn
        .prepare("SELECT value FROM settings WHERE key = ?1")
        .map_err(display_error)?;
    let value = stmt
        .query_row([key], |row| row.get::<_, String>(0))
        .unwrap_or_else(|_| default.to_string());
    Ok(value)
}

fn set_setting(state: &AppState, key: &str, value: &str) -> Result<(), String> {
    let conn = db(state).map_err(display_error)?;
    conn.execute(
        "INSERT OR REPLACE INTO settings(key, value, updated_at) VALUES (?1, ?2, ?3)",
        params![key, value, now_ts()],
    )
    .map_err(display_error)?;
    Ok(())
}

fn get_request_delay_ms(state: &AppState) -> Result<u64, String> {
    let raw = get_setting(
        state,
        "request_delay_ms",
        &DEFAULT_REQUEST_DELAY_MS.to_string(),
    )?;
    Ok(raw
        .parse::<u64>()
        .unwrap_or(DEFAULT_REQUEST_DELAY_MS)
        .clamp(0, 10_000))
}

fn validate_proxy_url(proxy_url: &str) -> Result<(), String> {
    if proxy_url.trim().is_empty() {
        return Ok(());
    }
    if !(proxy_url.starts_with("http://") || proxy_url.starts_with("https://")) {
        return Err("代理地址格式应类似 http://127.0.0.1:7890".to_string());
    }
    Ok(())
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn normalize_text(value: &str) -> String {
    let mut text = value.trim().to_lowercase();
    for (from, to) in [
        ("：", ":"),
        ("，", ","),
        ("（", "("),
        ("）", ")"),
        ("’", "'"),
        ("`", "'"),
    ] {
        text = text.replace(from, to);
    }
    Regex::new(r"\s+")
        .unwrap()
        .replace_all(&text, " ")
        .to_string()
}

fn clean_name(value: &str) -> String {
    let mut text = value
        .trim()
        .replace('：', ":")
        .replace('，', ",")
        .replace('（', "(")
        .replace('）', ")");
    text = text.replace('\u{3000}', " ");
    let bracket_re = Regex::new(r"\(([^()]*)\)").unwrap();
    loop {
        let old = text.clone();
        text = bracket_re
            .replace_all(&text, |caps: &regex::Captures| {
                if bracket_is_metadata(caps.get(1).map(|m| m.as_str()).unwrap_or("")) {
                    "".to_string()
                } else {
                    caps.get(0).unwrap().as_str().to_string()
                }
            })
            .to_string();
        if old == text {
            break;
        }
    }
    text = Regex::new(r"\b20\d{2}[./-]\d{1,2}([./-]\d{1,2})?\s*(到期|截止|过期)?")
        .unwrap()
        .replace_all(&text, "")
        .to_string();
    Regex::new(r"\s+")
        .unwrap()
        .replace_all(&text, " ")
        .trim_matches(|c: char| " -_|\t\r\n".contains(c))
        .to_string()
}

fn bracket_is_metadata(text: &str) -> bool {
    let lowered = normalize_text(text);
    let words = [
        "到期",
        "截止",
        "过期",
        "有效期",
        "expires",
        "expire",
        "expired",
        "steam",
        "key",
        "gift",
        "code",
        "兑换",
        "激活",
        "领取",
        "平台",
        "cn",
        "global",
    ];
    words.iter().any(|w| lowered.contains(w))
        || Regex::new(r"\b20\d{2}[./-]\d{1,2}([./-]\d{1,2})?\b")
            .unwrap()
            .is_match(&lowered)
        || Regex::new(r"^[\d./\-\s]+$").unwrap().is_match(&lowered)
}

fn cache_key(value: &str) -> String {
    let cleaned = normalize_text(&clean_name(value));
    let key = Regex::new(r"[^a-z0-9\u{4e00}-\u{9fff}]+")
        .unwrap()
        .replace_all(&cleaned, "")
        .to_string();
    if key.is_empty() {
        cleaned
    } else {
        key
    }
}

fn has_cjk(value: &str) -> bool {
    Regex::new(r"[\u{4e00}-\u{9fff}]").unwrap().is_match(value)
}

fn steam_search_language(query: &str) -> &'static str {
    if has_cjk(&clean_name(query)) {
        "schinese"
    } else {
        "english"
    }
}

fn confidence_for(score: i32) -> String {
    if score >= 88 {
        "高"
    } else if score >= 70 {
        "中"
    } else {
        "低"
    }
    .to_string()
}

fn title_score(query: &str, title: &str) -> i32 {
    let q = normalize_text(query);
    let t = normalize_text(&html_escape::decode_html_entities(title));
    if q.is_empty() || t.is_empty() {
        return 0;
    }
    let compact_re = Regex::new(r"[^a-z0-9\u{4e00}-\u{9fff}]+").unwrap();
    let cq = compact_re.replace_all(&q, "").to_string();
    let ct = compact_re.replace_all(&t, "").to_string();
    let ratio = strsim::normalized_levenshtein(&q, &t);
    let compact_ratio = strsim::normalized_levenshtein(&cq, &ct);
    let mut score = (ratio.max(compact_ratio) * 100.0).round() as i32;
    if !cq.is_empty() && ct.contains(&cq) {
        score = score.max(if cq.chars().count() >= 6 { 92 } else { 84 });
    }
    score.clamp(0, 100)
}

fn is_likely_non_game(title: &str, url: &str) -> bool {
    let lowered = format!(" {} {}", normalize_text(title), normalize_text(url));
    [
        " dlc",
        "soundtrack",
        "ost",
        "demo",
        "playtest",
        "dedicated server",
        "wallpaper",
        "artbook",
        " 试玩版",
        "图集",
        "原声音乐",
        "原声音乐集",
        "upgrade pack",
        "season pass",
        "expansion pass",
    ]
    .iter()
    .any(|word| lowered.contains(word))
}

fn throttle_steam_request(state: &AppState, url: &str) -> anyhow::Result<()> {
    if !url.contains("store.steampowered.com") {
        return Ok(());
    }
    let delay = Duration::from_millis(get_request_delay_ms(state).map_err(|e| anyhow!(e))?);
    if delay.is_zero() {
        return Ok(());
    }
    let mut last = state.last_steam_request_at.lock().unwrap();
    if let Some(prev) = *last {
        let elapsed = prev.elapsed();
        if elapsed < delay {
            thread::sleep(delay - elapsed);
        }
    }
    *last = Some(Instant::now());
    Ok(())
}

fn fetch_url(state: &AppState, url: &str, timeout_secs: u64) -> anyhow::Result<String> {
    let proxy_url = get_setting(state, "proxy_url", "").map_err(|e| anyhow!(e))?;
    fetch_url_with_proxy(state, url, timeout_secs, &proxy_url)
}

fn fetch_url_with_proxy(
    state: &AppState,
    url: &str,
    timeout_secs: u64,
    proxy_url: &str,
) -> anyhow::Result<String> {
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..5 {
        throttle_steam_request(state, url)?;
        let mut builder = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .user_agent(USER_AGENT)
            .danger_accept_invalid_certs(false);
        if !proxy_url.trim().is_empty() {
            builder = builder.proxy(Proxy::all(proxy_url.trim())?);
        }
        let client = builder.build()?;
        match client
            .get(url)
            .header("Accept-Language", "en-US,en;q=0.9,zh-CN;q=0.8")
            .header(
                "Cookie",
                "birthtime=315532801; lastagecheckage=1-January-1980; wants_mature_content=1; mature_content=1",
            )
            .send()
        {
            Ok(response) => {
                let status = response.status();
                if status.as_u16() == 429 && attempt < 4 {
                    let wait = response
                        .headers()
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(3 + attempt as u64 * 2);
                    thread::sleep(Duration::from_secs(wait.min(30)));
                    continue;
                }
                if !status.is_success() {
                    return Err(anyhow!(
                        "HTTP Error {}: {}",
                        status.as_u16(),
                        status.canonical_reason().unwrap_or("")
                    ));
                }
                return response.text().context("读取 Steam 响应失败");
            }
            Err(error) => {
                last_error = Some(error.into());
                if attempt < 4 {
                    let wait = (get_request_delay_ms(state).unwrap_or(DEFAULT_REQUEST_DELAY_MS)
                        / 1000)
                        .max(2)
                        * (attempt as u64 + 1);
                    thread::sleep(Duration::from_secs(wait.min(15)));
                    continue;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("请求失败")))
}

fn parse_search_results(page: &str, query: &str) -> Vec<Candidate> {
    let re = Regex::new(
        r#"(?is)<a[^>]+href="(?P<url>https://store\.steampowered\.com/app/(?P<app_id>\d+)/[^"?]+)[^"]*"[^>]*>(?P<body>.*?)</a>"#,
    )
    .unwrap();
    let title_re =
        Regex::new(r#"(?is)<span[^>]+class="[^"]*\btitle\b[^"]*"[^>]*>(?P<title>.*?)</span>"#)
            .unwrap();
    let suggest_title_re =
        Regex::new(r#"(?is)<div[^>]+class="[^"]*\bmatch_name\b[^"]*"[^>]*>(?P<title>.*?)</div>"#)
            .unwrap();
    let tag_re = Regex::new(r"(?is)<[^>]+>").unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    for caps in re.captures_iter(page) {
        let app_id = caps.name("app_id").unwrap().as_str().to_string();
        if seen.contains(&app_id) {
            continue;
        }
        let body = caps.name("body").unwrap().as_str();
        let raw_title = title_re
            .captures(body)
            .or_else(|| suggest_title_re.captures(body))
            .and_then(|title_caps| title_caps.name("title").map(|m| m.as_str()));
        let Some(raw_title) = raw_title else {
            continue;
        };
        let title = html_escape::decode_html_entities(&tag_re.replace_all(raw_title, ""))
            .trim()
            .to_string();
        let url = format!(
            "{}/",
            html_escape::decode_html_entities(caps.name("url").unwrap().as_str())
                .trim_end_matches('/')
        );
        if title.is_empty() || is_likely_non_game(&title, &url) {
            continue;
        }
        let score = title_score(query, &title);
        candidates.push(Candidate {
            app_id: app_id.clone(),
            title,
            url,
            score,
            confidence: confidence_for(score),
            kind: "游戏本体".to_string(),
        });
        seen.insert(app_id);
    }
    candidates.sort_by(|a, b| b.score.cmp(&a.score));
    candidates.truncate(8);
    candidates
}

fn get_search_cache(
    state: &AppState,
    query: &str,
    language: &str,
) -> anyhow::Result<Option<Vec<Candidate>>> {
    let key = format!("{}:{}:{}", SEARCH_CACHE_VERSION, language, cache_key(query));
    let conn = db(state)?;
    let mut stmt = conn.prepare("SELECT candidates_json FROM search_cache WHERE query_key = ?1")?;
    let result = stmt.query_row([key], |row| row.get::<_, String>(0));
    match result {
        Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save_search_cache(
    state: &AppState,
    query: &str,
    language: &str,
    candidates: &[Candidate],
) -> anyhow::Result<()> {
    let key = format!("{}:{}:{}", SEARCH_CACHE_VERSION, language, cache_key(query));
    let conn = db(state)?;
    conn.execute(
        "INSERT OR REPLACE INTO search_cache(query_key, query, candidates_json, updated_at) VALUES (?1, ?2, ?3, ?4)",
        params![key, query, serde_json::to_string(candidates)?, now_ts()],
    )?;
    Ok(())
}

fn search_steam(state: &AppState, query: &str) -> anyhow::Result<Vec<Candidate>> {
    let language = steam_search_language(query);
    if let Some(cached) = get_search_cache(state, query, language)? {
        if !cached.is_empty() {
            return Ok(cached);
        }
    }
    let url = format!(
        "https://store.steampowered.com/search/?term={}&category1=998&ndl=1&l={}",
        urlencoding::encode(query),
        language
    );
    let page = fetch_url(state, &url, 20)?;
    let mut candidates = parse_search_results(&page, query);
    if candidates
        .first()
        .map(|candidate| candidate.score < 80)
        .unwrap_or(true)
    {
        let suggest_url = format!(
            "https://store.steampowered.com/search/suggest?term={}&f=games&cc=US&realm=1&l={}",
            urlencoding::encode(query),
            language
        );
        let suggest_page = fetch_url(state, &suggest_url, 20)?;
        merge_candidates(&mut candidates, parse_search_results(&suggest_page, query));
    }
    save_search_cache(state, query, language, &candidates)?;
    Ok(candidates)
}

fn merge_candidates(candidates: &mut Vec<Candidate>, new_candidates: Vec<Candidate>) {
    for candidate in new_candidates {
        if let Some(existing) = candidates
            .iter_mut()
            .find(|existing| existing.app_id == candidate.app_id)
        {
            if candidate.score > existing.score {
                *existing = candidate;
            }
        } else {
            candidates.push(candidate);
        }
    }
    candidates.sort_by(|a, b| b.score.cmp(&a.score));
    candidates.truncate(8);
}

fn push_unique(values: &mut Vec<String>, value: String) {
    let value = value.trim().to_string();
    if value.is_empty() {
        return;
    }
    let normalized = normalize_text(&value);
    if !values.iter().any(|item| normalize_text(item) == normalized) {
        values.push(value);
    }
}

fn initial_query_variants(original: &str, cleaned: &str) -> Vec<String> {
    let mut queries = Vec::new();
    push_unique(&mut queries, cleaned.to_string());
    push_unique(&mut queries, original.to_string());
    queries
}

fn get_cached_match(state: &AppState, original: &str) -> anyhow::Result<Option<MatchResult>> {
    let key = cache_key(original);
    let conn = db(state)?;
    let result = conn.query_row(
        "SELECT result_json FROM matches WHERE cache_key = ?1",
        [key],
        |row| row.get::<_, String>(0),
    );
    match result {
        Ok(json) => {
            let mut item: MatchResult = serde_json::from_str(&json)?;
            if (item.status == "未找到" && item.source == "自动匹配")
                || item.status == "已推荐，需复核"
                || item.source == "自动推荐"
            {
                return Ok(None);
            }
            item.from_cache = true;
            Ok(Some(item))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save_match(
    state: &AppState,
    original: &str,
    cleaned: &str,
    result: &MatchResult,
    source: &str,
) -> Result<(), String> {
    let key = cache_key(original);
    let mut payload = result.clone();
    payload.from_cache = false;
    let conn = db(state).map_err(display_error)?;
    conn.execute(
        "INSERT OR REPLACE INTO matches(cache_key, original, cleaned, result_json, source, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![key, original, cleaned, serde_json::to_string(&payload).map_err(display_error)?, source, now_ts()],
    )
    .map_err(display_error)?;
    Ok(())
}

fn match_game_inner(original: &str, force: bool, state: &AppState) -> anyhow::Result<MatchResult> {
    let original = original.trim();
    let cleaned = clean_name(original);
    if original.is_empty() {
        return Ok(MatchResult {
            original: "".into(),
            cleaned,
            status: "未找到".into(),
            steam_title: "".into(),
            app_id: "".into(),
            url: "".into(),
            confidence: "低".into(),
            score: 0,
            needs_review: true,
            source: "自动匹配".into(),
            candidates: vec![],
            message: "空名称".into(),
            from_cache: false,
        });
    }
    if !force {
        if let Some(cached) = get_cached_match(state, original)? {
            return Ok(cached);
        }
    }
    let queries = initial_query_variants(original, &cleaned);
    let mut map: std::collections::HashMap<String, Candidate> = std::collections::HashMap::new();
    let mut errors = Vec::new();
    for query in queries {
        match search_steam(state, &query) {
            Ok(candidates) => {
                for candidate in candidates {
                    let replace = map
                        .get(&candidate.app_id)
                        .map(|old| candidate.score > old.score)
                        .unwrap_or(true);
                    if replace {
                        map.insert(candidate.app_id.clone(), candidate);
                    }
                }
            }
            Err(error) => errors.push(error.to_string()),
        }
    }
    let mut candidates: Vec<Candidate> = map.into_values().collect();
    candidates.sort_by(|a, b| b.score.cmp(&a.score));
    if candidates.is_empty() {
        let result = MatchResult {
            original: original.to_string(),
            cleaned,
            status: "未找到".into(),
            steam_title: "".into(),
            app_id: "".into(),
            url: "".into(),
            confidence: "低".into(),
            score: 0,
            needs_review: true,
            source: "自动匹配".into(),
            candidates: vec![],
            message: if errors.is_empty() {
                "没有找到候选".into()
            } else {
                format!("Steam 搜索失败：{}", errors.join("；"))
            },
            from_cache: false,
        };
        if errors.is_empty() {
            let _ = save_match(state, original, &result.cleaned, &result, "auto");
        }
        return Ok(result);
    }
    let best = candidates[0].clone();
    let second = candidates.get(1).map(|c| c.score).unwrap_or(0);
    let high = best.score >= 88 && best.score - second >= 5;
    let result = MatchResult {
        original: original.to_string(),
        cleaned,
        status: if high {
            "已匹配"
        } else {
            "已推荐，需复核"
        }
        .into(),
        steam_title: best.title,
        app_id: best.app_id,
        url: best.url,
        confidence: best.confidence,
        score: best.score,
        needs_review: !high,
        source: if high { "自动匹配" } else { "自动推荐" }.into(),
        candidates,
        message: "".into(),
        from_cache: false,
    };
    save_match(state, original, &result.cleaned, &result, "auto").map_err(|e| anyhow!(e))?;
    Ok(result)
}

fn normalize_steam_app_url(url: &str) -> anyhow::Result<(String, String)> {
    let re = Regex::new(r"store\.steampowered\.com/app/(\d+)(?:/([^?#]*))?").unwrap();
    let caps = re
        .captures(url.trim())
        .ok_or_else(|| anyhow!("链接必须是 Steam 游戏本体页面，例如 https://store.steampowered.com/app/1601740/Corpse_Keeper/"))?;
    let app_id = caps.get(1).unwrap().as_str().to_string();
    let slug = caps
        .get(2)
        .map(|m| m.as_str().trim_matches('/'))
        .unwrap_or("");
    let normalized = if slug.is_empty() {
        format!("https://store.steampowered.com/app/{}/", app_id)
    } else {
        format!("https://store.steampowered.com/app/{}/{}/", app_id, slug)
    };
    Ok((app_id, normalized))
}

fn fetch_app_title(state: &AppState, app_id: &str, url: &str) -> String {
    let api_url = format!(
        "https://store.steampowered.com/api/appdetails?appids={}&filters=basic&l=english",
        urlencoding::encode(app_id)
    );
    if let Ok(text) = fetch_url(state, &api_url, 15) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(name) = json
                .get(app_id)
                .and_then(|v| v.get("data"))
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str())
            {
                if !name.trim().is_empty() {
                    return name.trim().to_string();
                }
            }
        }
    }
    title_from_slug(url, app_id)
}

fn title_from_slug(url: &str, app_id: &str) -> String {
    let re = Regex::new(&format!(r"/app/{}/([^/?#]+)/?", regex::escape(app_id))).unwrap();
    if let Some(caps) = re.captures(url) {
        let slug = caps
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or("")
            .trim_matches(&['_', '-', '/', ' '][..]);
        if !slug.is_empty() {
            return slug.replace('_', " ").replace('-', " ");
        }
    }
    format!("Steam App {}", app_id)
}

fn manual_update_inner(req: ManualRequest, state: &AppState) -> anyhow::Result<MatchResult> {
    let cleaned = clean_name(&req.original);
    let (app_id, url) = normalize_steam_app_url(&req.url)?;
    let title = req
        .steam_title
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fetch_app_title(state, &app_id, &url));
    let result = MatchResult {
        original: req.original,
        cleaned,
        status: "已人工修正".into(),
        steam_title: title,
        app_id,
        url,
        confidence: "高".into(),
        score: 100,
        needs_review: false,
        source: if req.source.as_deref() == Some("手动粘贴") {
            "手动粘贴".into()
        } else {
            "人工修正".into()
        },
        candidates: req.candidates.unwrap_or_default(),
        message: "".into(),
        from_cache: false,
    };
    save_match(state, &result.original, &result.cleaned, &result, "manual")
        .map_err(|e| anyhow!(e))?;
    Ok(result)
}

fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let (text, _, _) = GBK.decode(bytes);
            text.into_owned()
        }
    }
}

fn parse_txt(bytes: &[u8]) -> Vec<String> {
    decode_text(bytes)
        .trim_start_matches('\u{feff}')
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn parse_csv(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    let text = decode_text(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(text.as_bytes());
    let mut names = Vec::new();
    for record in reader.records() {
        let record = record?;
        if let Some(value) = record.get(0) {
            let value = value.trim();
            if !value.is_empty() {
                names.push(value.to_string());
            }
        }
    }
    Ok(names)
}

fn parse_xlsx(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut shared = Vec::new();
    if let Ok(mut file) = archive.by_name("xl/sharedStrings.xml") {
        let mut xml = String::new();
        file.read_to_string(&mut xml)?;
        let re = Regex::new(r"(?is)<si>.*?</si>").unwrap();
        let text_re = Regex::new(r"(?is)<t[^>]*>(.*?)</t>").unwrap();
        for si in re.find_iter(&xml) {
            let mut value = String::new();
            for caps in text_re.captures_iter(si.as_str()) {
                value.push_str(&html_escape::decode_html_entities(
                    caps.get(1).unwrap().as_str(),
                ));
            }
            shared.push(value);
        }
    }
    let sheet_name = (1..=20)
        .map(|i| format!("xl/worksheets/sheet{}.xml", i))
        .find(|name| archive.by_name(name).is_ok())
        .ok_or_else(|| anyhow!("未找到 Excel 工作表"))?;
    let mut sheet = String::new();
    archive.by_name(&sheet_name)?.read_to_string(&mut sheet)?;
    let row_re = Regex::new(r"(?is)<row[^>]*>(.*?)</row>").unwrap();
    let cell_re = Regex::new(r#"(?is)<c\b(?P<attrs>[^>]*)>(?P<body>.*?)</c>"#).unwrap();
    let cell_ref_re = Regex::new(r#"r="(?P<ref>[^"]+)""#).unwrap();
    let cell_type_re = Regex::new(r#"t="(?P<t>[^"]+)""#).unwrap();
    let v_re = Regex::new(r"(?is)<v>(.*?)</v>").unwrap();
    let inline_re = Regex::new(r"(?is)<t[^>]*>(.*?)</t>").unwrap();
    let mut names = Vec::new();
    for row in row_re.captures_iter(&sheet) {
        let row_body = row.get(1).unwrap().as_str();
        let Some(cell) = cell_re.captures_iter(row_body).find(|cell| {
            let attrs = cell.name("attrs").map(|m| m.as_str()).unwrap_or("");
            let Some(cell_ref) = cell_ref_re
                .captures(attrs)
                .and_then(|caps| caps.name("ref"))
                .map(|m| m.as_str())
            else {
                return false;
            };
            cell_ref.starts_with('A') && cell_ref[1..].chars().all(|c| c.is_ascii_digit())
        }) else {
            continue;
        };
        let attrs = cell.name("attrs").map(|m| m.as_str()).unwrap_or("");
        let cell_type = cell_type_re
            .captures(attrs)
            .and_then(|caps| caps.name("t"))
            .map(|m| m.as_str())
            .unwrap_or("");
        let body = cell.name("body").unwrap().as_str();
        let value = if cell_type == "inlineStr" {
            inline_re
                .captures(body)
                .map(|c| html_escape::decode_html_entities(c.get(1).unwrap().as_str()).to_string())
                .unwrap_or_default()
        } else {
            let raw = v_re
                .captures(body)
                .map(|c| html_escape::decode_html_entities(c.get(1).unwrap().as_str()).to_string())
                .unwrap_or_default();
            if cell_type == "s" {
                raw.parse::<usize>()
                    .ok()
                    .and_then(|i| shared.get(i).cloned())
                    .unwrap_or_default()
            } else {
                raw
            }
        };
        if !value.trim().is_empty() {
            names.push(value.trim().to_string());
        }
    }
    Ok(names)
}

fn drop_known_header(names: Vec<String>) -> Vec<String> {
    if let Some(first) = names.first() {
        let header = normalize_text(first);
        let headers = [
            "name",
            "game",
            "games",
            "title",
            "titles",
            "游戏",
            "游戏名",
            "游戏名称",
            "名称",
        ];
        if headers.contains(&header.as_str()) {
            return names.into_iter().skip(1).collect();
        }
    }
    names
}

fn export_csv(path: &Path, results: &[MatchResult]) -> anyhow::Result<()> {
    let mut writer = csv::Writer::from_writer(vec![]);
    writer.write_record([
        "原始输入",
        "清洗后名称",
        "匹配状态",
        "Steam游戏名",
        "Steam App ID",
        "Steam链接",
        "置信度",
        "是否需要复核",
        "结果来源",
        "候选数量",
        "备注",
    ])?;
    for item in results {
        writer.write_record([
            item.original.as_str(),
            item.cleaned.as_str(),
            item.status.as_str(),
            item.steam_title.as_str(),
            item.app_id.as_str(),
            item.url.as_str(),
            item.confidence.as_str(),
            if item.needs_review { "是" } else { "否" },
            item.source.as_str(),
            &item.candidates.len().to_string(),
            item.message.as_str(),
        ])?;
    }
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    bytes.extend(writer.into_inner()?);
    fs::write(path, bytes)?;
    Ok(())
}

fn export_xlsx(path: &Path, results: &[MatchResult]) -> anyhow::Result<()> {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let worksheet = workbook.add_worksheet();
    let headers = [
        "原始输入",
        "清洗后名称",
        "匹配状态",
        "Steam游戏名",
        "Steam App ID",
        "Steam链接",
        "置信度",
        "是否需要复核",
        "结果来源",
        "候选数量",
        "备注",
    ];
    for (col, header) in headers.iter().enumerate() {
        worksheet.write_string(0, col as u16, *header)?;
    }
    for (row, item) in results.iter().enumerate() {
        let r = row as u32 + 1;
        worksheet.write_string(r, 0, &item.original)?;
        worksheet.write_string(r, 1, &item.cleaned)?;
        worksheet.write_string(r, 2, &item.status)?;
        worksheet.write_string(r, 3, &item.steam_title)?;
        worksheet.write_string(r, 4, &item.app_id)?;
        worksheet.write_string(r, 5, &item.url)?;
        worksheet.write_string(r, 6, &item.confidence)?;
        worksheet.write_string(r, 7, if item.needs_review { "是" } else { "否" })?;
        worksheet.write_string(r, 8, &item.source)?;
        worksheet.write_number(r, 9, item.candidates.len() as f64)?;
        worksheet.write_string(r, 10, &item.message)?;
    }
    workbook.save(path)?;
    Ok(())
}

fn is_portable_dir(data_dir: &Path) -> bool {
    data_dir
        .file_name()
        .and_then(|s| s.to_str())
        .map(|name| name.eq_ignore_ascii_case("data"))
        .unwrap_or(false)
}

fn resolve_data_dir(app: &tauri::App) -> PathBuf {
    let portable = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("portable.flag")))
        .filter(|p| p.exists())
        .and_then(|flag| flag.parent().map(|p| p.join("data")));
    if let Some(dir) = portable {
        if fs::create_dir_all(&dir).is_ok() && fs::write(dir.join(".write_test"), b"ok").is_ok() {
            let _ = fs::remove_file(dir.join(".write_test"));
            return dir;
        }
    }
    app.path().app_data_dir().unwrap_or_else(|_| {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("SteamLinkMatcher")
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = resolve_data_dir(app);
            fs::create_dir_all(&data_dir)?;
            let conn = Connection::open(db_path(&data_dir))?;
            init_db(&conn)?;
            app.manage(AppState {
                data_dir,
                last_steam_request_at: Arc::new(Mutex::new(None)),
            });
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            test_proxy,
            match_game,
            manual_update,
            confirm_result,
            clear_cache,
            import_file,
            export_file,
            open_external_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steam_suggest_results() {
        let html = r#"<a class="match" data-ds-appid="1398070" href="https://store.steampowered.com/app/1398070/The_Book_of_Bondmaids/?snr=1_7_15__13"><div class="match_name">The Book of Bondmaids</div></a>"#;
        let candidates = parse_search_results(html, "The Book of Bondmaids");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].app_id, "1398070");
        assert_eq!(candidates[0].title, "The Book of Bondmaids");
        assert_eq!(
            candidates[0].url,
            "https://store.steampowered.com/app/1398070/The_Book_of_Bondmaids/"
        );
        assert_eq!(candidates[0].score, 100);
    }

    #[test]
    fn merge_candidates_prefers_better_score() {
        let mut candidates = vec![Candidate {
            app_id: "1".to_string(),
            title: "Dragon Quest".to_string(),
            url: "https://store.steampowered.com/app/1/Dragon_Quest/".to_string(),
            score: 44,
            confidence: "低".to_string(),
            kind: "游戏本体".to_string(),
        }];
        merge_candidates(
            &mut candidates,
            vec![Candidate {
                app_id: "1554470".to_string(),
                title: "Dragon Island".to_string(),
                url: "https://store.steampowered.com/app/1554470/Dragon_Island/".to_string(),
                score: 100,
                confidence: "高".to_string(),
                kind: "游戏本体".to_string(),
            }],
        );
        assert_eq!(candidates[0].app_id, "1554470");
        assert_eq!(candidates[0].score, 100);
    }
}
