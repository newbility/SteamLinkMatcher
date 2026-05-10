from __future__ import annotations

import csv
import http.client
import html
import io
import json
import mimetypes
import os
import re
import socket
import sqlite3
import ssl
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from dataclasses import dataclass
from difflib import SequenceMatcher
from http import HTTPStatus
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent
WEB_DIR = ROOT / "web"
DATA_DIR = ROOT / "data"
DB_PATH = DATA_DIR / "cache.sqlite3"
USER_AGENT = "SteamLinkMatcher/0.1 (+local tool)"
DEFAULT_REQUEST_DELAY_MS = 1500
STEAM_REQUEST_LOCK = threading.Lock()
LAST_STEAM_REQUEST_AT = 0.0


METADATA_WORDS = (
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
)

NON_GAME_WORDS = (
    " dlc",
    "soundtrack",
    "ost",
    "demo",
    "playtest",
    "dedicated server",
    "wallpaper",
    "artbook",
    "upgrade pack",
    "season pass",
    "expansion pass",
)


@dataclass
class Candidate:
    app_id: str
    title: str
    url: str
    score: int
    confidence: str
    type: str = "游戏本体"

    def to_dict(self) -> dict[str, Any]:
        return {
            "appId": self.app_id,
            "title": self.title,
            "url": self.url,
            "score": self.score,
            "confidence": self.confidence,
            "type": self.type,
        }


def init_db() -> None:
    DATA_DIR.mkdir(exist_ok=True)
    with db_connect() as conn:
        conn.execute(
            """
            CREATE TABLE IF NOT EXISTS matches (
                cache_key TEXT PRIMARY KEY,
                original TEXT NOT NULL,
                cleaned TEXT NOT NULL,
                result_json TEXT NOT NULL,
                source TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            )
            """
        )
        conn.execute(
            """
            CREATE TABLE IF NOT EXISTS search_cache (
                query_key TEXT PRIMARY KEY,
                query TEXT NOT NULL,
                candidates_json TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            )
            """
        )
        conn.execute(
            """
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            )
            """
        )


def db_connect() -> sqlite3.Connection:
    stale_journal = Path(str(DB_PATH) + "-journal")
    if stale_journal.exists():
        try:
            stale_journal.unlink()
        except OSError:
            pass
    conn = sqlite3.connect(DB_PATH, timeout=30, isolation_level=None)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")
    return conn


def get_setting(key: str, default: str = "") -> str:
    with db_connect() as conn:
        row = conn.execute("SELECT value FROM settings WHERE key = ?", (key,)).fetchone()
    return row[0] if row else default


def set_setting(key: str, value: str) -> None:
    with db_connect() as conn:
        conn.execute(
            """
            INSERT OR REPLACE INTO settings(key, value, updated_at)
            VALUES (?, ?, ?)
            """,
            (key, value, int(time.time())),
        )


def get_proxy_url() -> str:
    return get_setting("proxy_url", "").strip()


def get_request_delay_ms() -> int:
    raw = get_setting("request_delay_ms", str(DEFAULT_REQUEST_DELAY_MS)).strip()
    try:
        value = int(raw)
    except ValueError:
        value = DEFAULT_REQUEST_DELAY_MS
    return max(0, min(value, 10000))


def set_request_delay_ms(value: Any) -> int:
    try:
        delay = int(value)
    except (TypeError, ValueError):
        delay = DEFAULT_REQUEST_DELAY_MS
    delay = max(0, min(delay, 10000))
    set_setting("request_delay_ms", str(delay))
    return delay


def validate_proxy_url(proxy_url: str) -> None:
    if not proxy_url:
        return
    parsed = urllib.parse.urlparse(proxy_url)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise ValueError("代理地址格式应类似 http://127.0.0.1:7890")


def throttle_steam_request(url: str) -> None:
    global LAST_STEAM_REQUEST_AT
    if "store.steampowered.com" not in url:
        return
    delay = get_request_delay_ms() / 1000
    if delay <= 0:
        return
    with STEAM_REQUEST_LOCK:
        now = time.monotonic()
        wait = LAST_STEAM_REQUEST_AT + delay - now
        if wait > 0:
            time.sleep(wait)
        LAST_STEAM_REQUEST_AT = time.monotonic()


def read_json_body(handler: SimpleHTTPRequestHandler) -> dict[str, Any]:
    length = int(handler.headers.get("Content-Length") or "0")
    if length <= 0:
        return {}
    raw = handler.rfile.read(length)
    return json.loads(raw.decode("utf-8"))


def send_json(handler: SimpleHTTPRequestHandler, payload: Any, status: int = 200) -> None:
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json; charset=utf-8")
    handler.send_header("Content-Length", str(len(body)))
    handler.end_headers()
    handler.wfile.write(body)


def send_bytes(
    handler: SimpleHTTPRequestHandler,
    payload: bytes,
    content_type: str,
    filename: str | None = None,
) -> None:
    handler.send_response(HTTPStatus.OK)
    handler.send_header("Content-Type", content_type)
    handler.send_header("Content-Length", str(len(payload)))
    if filename:
        quoted = urllib.parse.quote(filename)
        handler.send_header("Content-Disposition", f"attachment; filename*=UTF-8''{quoted}")
    handler.end_headers()
    handler.wfile.write(payload)


def normalize_text(value: str) -> str:
    value = value.strip().lower()
    value = value.replace("：", ":").replace("，", ",").replace("（", "(").replace("）", ")")
    value = value.replace("’", "'").replace("`", "'").replace("“", '"').replace("”", '"')
    value = re.sub(r"\s+", " ", value)
    return value


def cache_key(value: str) -> str:
    value = clean_name(value)
    value = normalize_text(value)
    value = re.sub(r"[^a-z0-9\u4e00-\u9fff]+", "", value)
    return value or normalize_text(value)


def has_cjk(value: str) -> bool:
    return bool(re.search(r"[\u4e00-\u9fff]", value))


def steam_search_language(query: str) -> str:
    return "schinese" if has_cjk(clean_name(query)) else "english"


def bracket_is_metadata(text: str) -> bool:
    lowered = normalize_text(text)
    if any(word in lowered for word in METADATA_WORDS):
        return True
    if re.search(r"\b20\d{2}[./-]\d{1,2}([./-]\d{1,2})?\b", lowered):
        return True
    if re.fullmatch(r"[\d./\-\s]+", lowered):
        return True
    return False


def clean_name(value: str) -> str:
    text = value.strip()
    text = text.replace("：", ":").replace("，", ",").replace("（", "(").replace("）", ")")
    text = text.replace("\u3000", " ")

    def strip_metadata(match: re.Match[str]) -> str:
        inner = match.group(1).strip()
        return "" if bracket_is_metadata(inner) else match.group(0)

    old = None
    while old != text:
        old = text
        text = re.sub(r"\(([^()]*)\)", strip_metadata, text)

    text = re.sub(r"\b20\d{2}[./-]\d{1,2}([./-]\d{1,2})?\s*(到期|截止|过期)?", "", text)
    text = re.sub(r"\s+", " ", text)
    return text.strip(" -_|\t\r\n")


def confidence_for(score: int) -> str:
    if score >= 88:
        return "高"
    if score >= 70:
        return "中"
    return "低"


def is_likely_non_game(title: str, url: str) -> bool:
    lowered = f" {normalize_text(title)} {normalize_text(url)}"
    return any(word in lowered for word in NON_GAME_WORDS)


def title_score(query: str, title: str) -> int:
    q = normalize_text(query)
    t = normalize_text(html.unescape(title))
    if not q or not t:
        return 0
    compact_q = re.sub(r"[^a-z0-9\u4e00-\u9fff]+", "", q)
    compact_t = re.sub(r"[^a-z0-9\u4e00-\u9fff]+", "", t)
    ratio = SequenceMatcher(None, q, t).ratio()
    compact_ratio = SequenceMatcher(None, compact_q, compact_t).ratio()
    score = int(max(ratio, compact_ratio) * 100)
    if compact_q and compact_q in compact_t:
        score = max(score, 92 if len(compact_q) >= 6 else 84)
    if is_likely_non_game(title, ""):
        score -= 25
    return max(0, min(score, 100))


def fetch_url(url: str, timeout: int = 20) -> str:
    request = urllib.request.Request(
        url,
        headers={
            "User-Agent": USER_AGENT,
            "Accept-Language": "en-US,en;q=0.9,zh-CN;q=0.8",
        },
    )
    last_error: Exception | None = None
    max_attempts = 5
    for attempt in range(max_attempts):
        throttle_steam_request(url)
        try:
            proxy_url = get_proxy_url()
            if proxy_url:
                proxy_handler = urllib.request.ProxyHandler({"http": proxy_url, "https": proxy_url})
                opener = urllib.request.build_opener(proxy_handler)
                response_context = opener.open(request, timeout=timeout)
            else:
                response_context = urllib.request.urlopen(request, timeout=timeout)
            with response_context as response:
                encoding = response.headers.get_content_charset() or "utf-8"
                return response.read().decode(encoding, errors="replace")
        except urllib.error.HTTPError as exc:
            last_error = exc
            if exc.code != 429 or attempt >= max_attempts - 1:
                raise
            retry_after = exc.headers.get("Retry-After")
            try:
                wait = float(retry_after) if retry_after else 0
            except ValueError:
                wait = 0
            if wait <= 0:
                wait = max(3, (get_request_delay_ms() / 1000) * (attempt + 2))
            time.sleep(min(wait, 30))
        except (
            TimeoutError,
            socket.timeout,
            http.client.IncompleteRead,
            http.client.RemoteDisconnected,
            ssl.SSLError,
            ConnectionResetError,
            urllib.error.URLError,
        ) as exc:
            last_error = exc
            if attempt >= max_attempts - 1:
                raise
            wait = max(2, (get_request_delay_ms() / 1000) * (attempt + 1))
            time.sleep(min(wait, 15))
    if last_error:
        raise last_error
    raise RuntimeError("请求失败")


def parse_search_results(page: str, query: str) -> list[Candidate]:
    candidates: list[Candidate] = []
    seen: set[str] = set()
    pattern = re.compile(
        r'<a[^>]+href="(?P<url>https://store\.steampowered\.com/app/(?P<app_id>\d+)/[^"?]+)[^"]*"[^>]*>'
        r"(?P<body>.*?)</a>",
        re.IGNORECASE | re.DOTALL,
    )
    title_pattern = re.compile(r'<span[^>]+class="[^"]*\btitle\b[^"]*"[^>]*>(?P<title>.*?)</span>', re.I | re.S)
    for match in pattern.finditer(page):
        app_id = match.group("app_id")
        if app_id in seen:
            continue
        title_match = title_pattern.search(match.group("body"))
        if not title_match:
            continue
        title = re.sub(r"<[^>]+>", "", title_match.group("title"))
        title = html.unescape(title).strip()
        url = html.unescape(match.group("url")).rstrip("/") + "/"
        if not title or is_likely_non_game(title, url):
            continue
        score = title_score(query, title)
        candidates.append(
            Candidate(
                app_id=app_id,
                title=title,
                url=url,
                score=score,
                confidence=confidence_for(score),
            )
        )
        seen.add(app_id)
    candidates.sort(key=lambda item: item.score, reverse=True)
    return candidates[:8]


def normalize_steam_app_url(url: str) -> tuple[str, str]:
    value = url.strip()
    if not value:
        raise ValueError("请输入 Steam 链接")
    match = re.search(r"store\.steampowered\.com/app/(\d+)(?:/([^?#]*))?", value)
    if not match:
        raise ValueError("链接必须是 Steam 游戏本体页面，例如 https://store.steampowered.com/app/1601740/Corpse_Keeper/")
    app_id = match.group(1)
    slug = (match.group(2) or "").strip("/")
    normalized = f"https://store.steampowered.com/app/{app_id}/"
    if slug:
        normalized += f"{slug}/"
    return app_id, normalized


def extract_app_title(page: str) -> str:
    patterns = (
        r'<div[^>]+class="[^"]*\bapphub_AppName\b[^"]*"[^>]*>(?P<title>.*?)</div>',
        r'<meta[^>]+property="og:title"[^>]+content="(?P<title>[^"]+)"',
        r"<title>(?P<title>.*?)</title>",
    )
    for pattern in patterns:
        match = re.search(pattern, page, re.IGNORECASE | re.DOTALL)
        if not match:
            continue
        title = re.sub(r"<[^>]+>", "", match.group("title"))
        title = html.unescape(title).strip()
        title = re.sub(r"\s+on Steam\s*$", "", title, flags=re.I)
        if title:
            return title
    return ""


def title_from_slug(url: str, app_id: str) -> str:
    match = re.search(rf"/app/{re.escape(app_id)}/([^/?#]+)/?", url)
    if not match:
        return f"Steam App {app_id}"
    slug = urllib.parse.unquote(match.group(1)).strip("_-/ ")
    if not slug:
        return f"Steam App {app_id}"
    return re.sub(r"\s+", " ", slug.replace("_", " ").replace("-", " ")).strip()


def fetch_app_title(app_id: str, url: str) -> str:
    api_url = f"https://store.steampowered.com/api/appdetails?appids={urllib.parse.quote(app_id)}&filters=basic&l=english"
    last_error = ""
    try:
        payload = json.loads(fetch_url(api_url, timeout=15))
        item = payload.get(app_id) or {}
        title = str(((item.get("data") or {}).get("name") or "")).strip()
        if title:
            return title
    except Exception as exc:  # noqa: BLE001 - manual correction should still have a slug fallback.
        last_error = str(exc)

    try:
        page = fetch_url(url, timeout=20)
        title = extract_app_title(page)
        if title:
            return title
    except Exception as exc:  # noqa: BLE001 - converted to fallback title below.
        last_error = str(exc)

    title = title_from_slug(url, app_id)
    if title.startswith("Steam App ") and last_error:
        raise ValueError(f"未能读取 Steam 游戏名：{last_error}")
    return title


def get_search_cache(query: str, language: str) -> list[dict[str, Any]] | None:
    key = f"{language}:{cache_key(query)}"
    with db_connect() as conn:
        row = conn.execute("SELECT candidates_json FROM search_cache WHERE query_key = ?", (key,)).fetchone()
    if not row:
        return None
    return json.loads(row[0])


def save_search_cache(query: str, language: str, candidates: list[Candidate]) -> None:
    key = f"{language}:{cache_key(query)}"
    with db_connect() as conn:
        conn.execute(
            """
            INSERT OR REPLACE INTO search_cache(query_key, query, candidates_json, updated_at)
            VALUES (?, ?, ?, ?)
            """,
            (key, query, json.dumps([item.to_dict() for item in candidates], ensure_ascii=False), int(time.time())),
        )


def search_steam(query: str) -> list[dict[str, Any]]:
    language = steam_search_language(query)
    cached = get_search_cache(query, language)
    if cached is not None:
        return cached
    params = urllib.parse.urlencode(
        {
            "term": query,
            "category1": "998",
            "ndl": "1",
            "l": language,
        }
    )
    url = f"https://store.steampowered.com/search/?{params}"
    page = fetch_url(url)
    candidates = parse_search_results(page, query)
    save_search_cache(query, language, candidates)
    return [item.to_dict() for item in candidates]


def get_cached_match(original: str) -> dict[str, Any] | None:
    key = cache_key(original)
    with db_connect() as conn:
        row = conn.execute("SELECT result_json FROM matches WHERE cache_key = ?", (key,)).fetchone()
    if not row:
        return None
    result = json.loads(row[0])
    if result.get("status") == "未找到" and "Steam 搜索失败" in str(result.get("message", "")):
        return None
    result["fromCache"] = True
    return result


def save_match(original: str, cleaned: str, result: dict[str, Any], source: str) -> None:
    key = cache_key(original)
    payload = dict(result)
    payload["fromCache"] = False
    with db_connect() as conn:
        conn.execute(
            """
            INSERT OR REPLACE INTO matches(cache_key, original, cleaned, result_json, source, updated_at)
            VALUES (?, ?, ?, ?, ?, ?)
            """,
            (key, original, cleaned, json.dumps(payload, ensure_ascii=False), source, int(time.time())),
        )


def match_game(original: str, force: bool = False) -> dict[str, Any]:
    original = original.strip()
    cleaned = clean_name(original)
    if not original:
        return {
            "original": original,
            "cleaned": cleaned,
            "status": "未找到",
            "steamTitle": "",
            "appId": "",
            "url": "",
            "confidence": "低",
            "score": 0,
            "needsReview": True,
            "source": "自动匹配",
            "candidates": [],
            "message": "空名称",
            "fromCache": False,
        }
    if not force:
        cached = get_cached_match(original)
        if cached is not None:
            return cached

    queries = [cleaned]
    if normalize_text(cleaned) != normalize_text(original):
        queries.append(original)

    candidate_map: dict[str, dict[str, Any]] = {}
    errors: list[str] = []
    for query in queries:
        try:
            for candidate in search_steam(query):
                current = candidate_map.get(candidate["appId"])
                if not current or candidate["score"] > current["score"]:
                    candidate_map[candidate["appId"]] = candidate
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            errors.append(str(exc))

    candidates = sorted(candidate_map.values(), key=lambda item: item.get("score", 0), reverse=True)
    if not candidates:
        result = {
            "original": original,
            "cleaned": cleaned,
            "status": "未找到",
            "steamTitle": "",
            "appId": "",
            "url": "",
            "confidence": "低",
            "score": 0,
            "needsReview": True,
            "source": "自动匹配",
            "candidates": [],
            "message": "没有找到候选" if not errors else "Steam 搜索失败：" + "；".join(errors[:2]),
            "fromCache": False,
        }
        if not errors:
            save_match(original, cleaned, result, "auto")
        return result

    best = candidates[0]
    second_score = candidates[1]["score"] if len(candidates) > 1 else 0
    high_confidence = best["score"] >= 88 and best["score"] - second_score >= 5
    result = {
        "original": original,
        "cleaned": cleaned,
        "status": "已匹配" if high_confidence else "已推荐，需复核",
        "steamTitle": best["title"],
        "appId": best["appId"],
        "url": best["url"],
        "confidence": best["confidence"],
        "score": best["score"],
        "needsReview": not high_confidence,
        "source": "自动匹配" if high_confidence else "自动推荐",
        "candidates": candidates,
        "message": "",
        "fromCache": False,
    }
    save_match(original, cleaned, result, "auto")
    return result


def parse_txt(raw: bytes) -> list[str]:
    text = raw.decode("utf-8-sig", errors="replace")
    return [line.strip() for line in text.splitlines() if line.strip()]


def drop_known_header(names: list[str]) -> list[str]:
    if not names:
        return names
    first = normalize_text(names[0])
    headers = {"name", "game", "games", "title", "titles", "游戏", "游戏名", "游戏名称", "名称"}
    return names[1:] if first in headers else names


def parse_csv(raw: bytes) -> list[str]:
    text = raw.decode("utf-8-sig", errors="replace")
    sample = text[:2048]
    dialect = csv.Sniffer().sniff(sample) if "," in sample or "\t" in sample or ";" in sample else csv.excel
    reader = csv.reader(io.StringIO(text), dialect)
    names: list[str] = []
    for row in reader:
        if row and row[0].strip():
            names.append(row[0].strip())
    return drop_known_header(names)


def xlsx_column_name(cell_ref: str) -> str:
    match = re.match(r"([A-Z]+)", cell_ref)
    return match.group(1) if match else ""


def parse_xlsx(raw: bytes) -> list[str]:
    ns = {"main": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
    names: list[str] = []
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        shared: list[str] = []
        if "xl/sharedStrings.xml" in archive.namelist():
            root = ET.fromstring(archive.read("xl/sharedStrings.xml"))
            for item in root.findall("main:si", ns):
                shared.append("".join(text.text or "" for text in item.findall(".//main:t", ns)))

        sheet_name = "xl/worksheets/sheet1.xml"
        if sheet_name not in archive.namelist():
            sheets = [name for name in archive.namelist() if name.startswith("xl/worksheets/sheet") and name.endswith(".xml")]
            if not sheets:
                return []
            sheet_name = sorted(sheets)[0]

        sheet = ET.fromstring(archive.read(sheet_name))
        for row in sheet.findall(".//main:row", ns):
            value = ""
            for cell in row.findall("main:c", ns):
                if xlsx_column_name(cell.attrib.get("r", "")) != "A":
                    continue
                cell_type = cell.attrib.get("t")
                if cell_type == "inlineStr":
                    value = "".join(text.text or "" for text in cell.findall(".//main:t", ns))
                else:
                    raw_value = cell.findtext("main:v", default="", namespaces=ns)
                    if cell_type == "s" and raw_value:
                        value = shared[int(raw_value)] if int(raw_value) < len(shared) else ""
                    else:
                        value = raw_value
                break
            if value.strip():
                names.append(value.strip())
    return drop_known_header(names)


def parse_import(filename: str, raw: bytes) -> list[str]:
    suffix = Path(filename).suffix.lower()
    if suffix == ".xlsx":
        return parse_xlsx(raw)
    if suffix == ".csv":
        return parse_csv(raw)
    return parse_txt(raw)


def make_csv(results: list[dict[str, Any]]) -> bytes:
    output = io.StringIO(newline="")
    writer = csv.writer(output)
    writer.writerow(["原始输入", "清洗后名称", "匹配状态", "Steam游戏名", "Steam App ID", "Steam链接", "置信度", "是否需要复核", "结果来源", "候选数量", "备注"])
    for item in results:
        writer.writerow(
            [
                item.get("original", ""),
                item.get("cleaned", ""),
                item.get("status", ""),
                item.get("steamTitle", ""),
                item.get("appId", ""),
                item.get("url", ""),
                item.get("confidence", ""),
                "是" if item.get("needsReview") else "否",
                item.get("source", ""),
                len(item.get("candidates") or []),
                item.get("message", ""),
            ]
        )
    return ("\ufeff" + output.getvalue()).encode("utf-8")


def cell_ref(col: int, row: int) -> str:
    letters = ""
    while col:
        col, rem = divmod(col - 1, 26)
        letters = chr(65 + rem) + letters
    return f"{letters}{row}"


def make_sheet_xml(rows: list[list[Any]]) -> str:
    lines = [
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>',
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">',
        "<sheetData>",
    ]
    for row_idx, row in enumerate(rows, start=1):
        lines.append(f'<row r="{row_idx}">')
        for col_idx, value in enumerate(row, start=1):
            ref = cell_ref(col_idx, row_idx)
            text = html.escape(str(value if value is not None else ""))
            lines.append(f'<c r="{ref}" t="inlineStr"><is><t>{text}</t></is></c>')
        lines.append("</row>")
    lines.extend(["</sheetData>", "</worksheet>"])
    return "".join(lines)


def make_xlsx(results: list[dict[str, Any]]) -> bytes:
    rows: list[list[Any]] = [["原始输入", "清洗后名称", "匹配状态", "Steam游戏名", "Steam App ID", "Steam链接", "置信度", "是否需要复核", "结果来源", "候选数量", "备注"]]
    for item in results:
        rows.append(
            [
                item.get("original", ""),
                item.get("cleaned", ""),
                item.get("status", ""),
                item.get("steamTitle", ""),
                item.get("appId", ""),
                item.get("url", ""),
                item.get("confidence", ""),
                "是" if item.get("needsReview") else "否",
                item.get("source", ""),
                len(item.get("candidates") or []),
                item.get("message", ""),
            ]
        )
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(
            "[Content_Types].xml",
            """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>""",
        )
        archive.writestr(
            "_rels/.rels",
            """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>""",
        )
        archive.writestr(
            "xl/workbook.xml",
            """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Steam链接匹配结果" sheetId="1" r:id="rId1"/></sheets>
</workbook>""",
        )
        archive.writestr(
            "xl/_rels/workbook.xml.rels",
            """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>""",
        )
        archive.writestr("xl/worksheets/sheet1.xml", make_sheet_xml(rows))
    return buffer.getvalue()


def manual_update(payload: dict[str, Any]) -> dict[str, Any]:
    original = str(payload.get("original", "")).strip()
    cleaned = clean_name(original)
    app_id, url = normalize_steam_app_url(str(payload.get("url", "")).strip())
    title = str(payload.get("steamTitle") or payload.get("title") or "").strip()
    if not title:
        title = fetch_app_title(app_id, url)
    result = {
        "original": original,
        "cleaned": cleaned,
        "status": "已人工修正",
        "steamTitle": title,
        "appId": app_id,
        "url": url,
        "confidence": "高",
        "score": 100,
        "needsReview": False,
        "source": "人工修正" if payload.get("source") != "手动粘贴" else "手动粘贴",
        "candidates": payload.get("candidates") or [],
        "message": "",
        "fromCache": False,
    }
    save_match(original, cleaned, result, "manual")
    return result


def confirm_result(payload: dict[str, Any]) -> dict[str, Any]:
    original = str(payload.get("original", "")).strip()
    cleaned = str(payload.get("cleaned") or clean_name(original)).strip()
    url = str(payload.get("url", "")).strip()
    if not original or not url:
        raise ValueError("只能确认已有链接的记录")
    result = {
        "original": original,
        "cleaned": cleaned,
        "status": "已匹配",
        "steamTitle": str(payload.get("steamTitle", "")).strip(),
        "appId": str(payload.get("appId", "")).strip(),
        "url": url,
        "confidence": str(payload.get("confidence") or "高"),
        "score": payload.get("score") or 100,
        "needsReview": False,
        "source": "人工确认",
        "candidates": payload.get("candidates") or [],
        "message": "",
        "fromCache": False,
    }
    save_match(original, cleaned, result, "manual")
    return result


class AppHandler(SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: Any) -> None:
        sys.stderr.write("%s - - [%s] %s\n" % (self.address_string(), self.log_date_time_string(), format % args))

    def translate_path(self, path: str) -> str:
        parsed = urllib.parse.urlparse(path)
        clean_path = parsed.path
        if clean_path == "/":
            clean_path = "/index.html"
        target = (WEB_DIR / clean_path.lstrip("/")).resolve()
        try:
            target.relative_to(WEB_DIR.resolve())
        except ValueError:
            return str(WEB_DIR / "__not_found__")
        return str(target)

    def do_GET(self) -> None:
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path == "/api/settings":
            send_json(self, {"proxyUrl": get_proxy_url(), "requestDelayMs": get_request_delay_ms()})
            return
        if self.path.startswith("/api/"):
            send_json(self, {"error": "Not found"}, 404)
            return
        return super().do_GET()

    def do_POST(self) -> None:
        parsed = urllib.parse.urlparse(self.path)
        try:
            if parsed.path == "/api/import":
                params = urllib.parse.parse_qs(parsed.query)
                filename = params.get("filename", ["input.txt"])[0]
                length = int(self.headers.get("Content-Length") or "0")
                raw = self.rfile.read(length)
                names = parse_import(filename, raw)
                send_json(self, {"names": names})
                return
            if parsed.path == "/api/match":
                payload = read_json_body(self)
                result = match_game(str(payload.get("name", "")), bool(payload.get("force")))
                send_json(self, result)
                return
            if parsed.path == "/api/manual":
                result = manual_update(read_json_body(self))
                send_json(self, result)
                return
            if parsed.path == "/api/confirm":
                result = confirm_result(read_json_body(self))
                send_json(self, result)
                return
            if parsed.path == "/api/export/csv":
                payload = read_json_body(self)
                send_bytes(self, make_csv(payload.get("results") or []), "text/csv; charset=utf-8", "steam_links_result.csv")
                return
            if parsed.path == "/api/export/xlsx":
                payload = read_json_body(self)
                send_bytes(
                    self,
                    make_xlsx(payload.get("results") or []),
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    "steam_links_result.xlsx",
                )
                return
            if parsed.path == "/api/cache/clear":
                payload = read_json_body(self)
                mode = str(payload.get("mode", "all")).strip()
                with db_connect() as conn:
                    conn.execute("DELETE FROM search_cache")
                    if mode == "all":
                        conn.execute("DELETE FROM matches")
                    elif mode != "search":
                        raise ValueError("缓存清理模式只能是 all 或 search")
                send_json(self, {"ok": True, "mode": mode})
                return
            if parsed.path == "/api/settings":
                payload = read_json_body(self)
                proxy_url = str(payload.get("proxyUrl", "")).strip()
                validate_proxy_url(proxy_url)
                set_setting("proxy_url", proxy_url)
                if "requestDelayMs" in payload:
                    request_delay_ms = set_request_delay_ms(payload.get("requestDelayMs"))
                else:
                    request_delay_ms = get_request_delay_ms()
                send_json(self, {"ok": True, "proxyUrl": proxy_url, "requestDelayMs": request_delay_ms})
                return
            if parsed.path == "/api/proxy/test":
                proxy_url = str(read_json_body(self).get("proxyUrl", "")).strip()
                validate_proxy_url(proxy_url)
                current_proxy = get_proxy_url()
                set_setting("proxy_url", proxy_url)
                try:
                    page = fetch_url("https://store.steampowered.com/search/?term=Corpse%20Keeper&category1=998&ndl=1&l=english", timeout=12)
                    ok = "Corpse Keeper" in page or "search_results" in page
                    send_json(self, {"ok": ok, "message": "代理测试成功" if ok else "已连接，但返回内容不像 Steam 搜索页"})
                finally:
                    set_setting("proxy_url", current_proxy)
                return
            send_json(self, {"error": "Not found"}, 404)
        except Exception as exc:  # noqa: BLE001 - convert local app failures to JSON for the UI.
            send_json(self, {"error": str(exc)}, 500)


def main() -> None:
    init_db()
    port = int(os.environ.get("PORT", "8765"))
    server = ThreadingHTTPServer(("127.0.0.1", port), AppHandler)
    print(f"Steam Link Matcher running at http://127.0.0.1:{port}")
    server.serve_forever()


if __name__ == "__main__":
    main()
