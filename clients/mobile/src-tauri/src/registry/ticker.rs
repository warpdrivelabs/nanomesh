use serde_json::{Value, json};

use super::http::{https_get_json, https_get_raw};

fn rate_num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn ticker_row(label: &str, value: &str, hint: &str, url: &str) -> Value {
    json!({ "label": label, "value": value, "hint": hint, "url": url })
}

fn ticker_fx() -> Option<Value> {
    let url = "https://open.er-api.com/v6/latest/USD";
    let v = https_get_json(url).ok()?;
    let rates = v.get("rates")?;
    let cny = rate_num(rates.get("CNY")?)?;
    let eur = rate_num(rates.get("EUR")?)?;
    let gbp = rate_num(rates.get("GBP")?)?;
    let jpy = rate_num(rates.get("JPY")?)?;
    if eur <= 0.0 || gbp <= 0.0 || jpy <= 0.0 {
        return None;
    }
    let eur_cny = cny / eur;
    let gbp_cny = cny / gbp;
    let jpy_cny = cny / jpy * 100.0;
    let note = v
        .get("time_last_update_utc")
        .and_then(|t| t.as_str())
        .unwrap_or("");
    Some(json!({
        "id": "fx",
        "title": "汇率",
        "label": format!("汇率  美元/人民币 {cny:.2}  欧元/人民币 {eur_cny:.2}  英镑/人民币 {gbp_cny:.2}  100日元/人民币 {jpy_cny:.2}"),
        "source": "ExchangeRate-API",
        "url": url,
        "note": note,
        "rows": [
            ticker_row("美元/人民币", &format!("{cny:.4}"), "1 美元", url),
            ticker_row("欧元/人民币", &format!("{eur_cny:.4}"), "1 欧元", url),
            ticker_row("英镑/人民币", &format!("{gbp_cny:.4}"), "1 英镑", url),
            ticker_row("100日元/人民币", &format!("{jpy_cny:.4}"), "100 日元", url),
        ],
    }))
}

fn gate_ticker(pair: &str) -> Option<Value> {
    let v = https_get_json(&format!(
        "https://api.gateio.ws/api/v4/spot/tickers?currency_pair={pair}"
    ))
    .ok()?;
    v.as_array()?.first().cloned()
}

fn gate_num(row: &Value, key: &str) -> Option<f64> {
    row.get(key).and_then(rate_num)
}

fn ticker_crypto() -> Option<Value> {
    let pairs = [
        ("BTC", "BTC_USDT"),
        ("ETH", "ETH_USDT"),
        ("SOL", "SOL_USDT"),
    ];
    let mut rows = Vec::new();
    let mut bits = Vec::new();
    for (name, pair) in pairs {
        let Some(row) = gate_ticker(pair) else {
            continue;
        };
        let Some(last) = gate_num(&row, "last") else {
            continue;
        };
        let page = format!("https://www.gate.io/trade/{pair}");
        let change = row
            .get("change_percentage")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let mut hint = Vec::new();
        if let Some(hi) = gate_num(&row, "high_24h") {
            hint.push(format!("24h 高 {hi:.2}"));
        }
        if let Some(lo) = gate_num(&row, "low_24h") {
            hint.push(format!("24h 低 {lo:.2}"));
        }
        let value = if name == "BTC" || name == "ETH" {
            format!("{last:.0} 美元")
        } else {
            format!("{last:.2} 美元")
        };
        let change_txt = if change.is_empty() {
            String::new()
        } else {
            format!("  {change}%")
        };
        bits.push(format!("{name} {value}{change_txt}"));
        let shown = if change.is_empty() {
            value
        } else {
            format!("{value}  {change}%")
        };
        rows.push(ticker_row(name, &shown, &hint.join("  "), &page));
    }
    if rows.is_empty() {
        return None;
    }
    let first = rows[0]
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or("https://www.gate.io");
    Some(json!({
        "id": "crypto",
        "title": "数字货币",
        "label": format!("币价  {}", bits.join("  ")),
        "source": "Gate.io",
        "url": first,
        "note": "现货最新价",
        "rows": rows,
    }))
}

fn rss_text(block: &str, tag: &str) -> Option<String> {
    let raw = block
        .split(&format!("<{tag}>"))
        .nth(1)?
        .split(&format!("</{tag}>"))
        .next()?
        .trim();
    let text = raw
        .trim_start_matches("<![CDATA[")
        .trim_end_matches("]]>")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn ticker_news() -> Vec<Value> {
    let Ok((_code, body)) = https_get_raw("https://www.chinanews.com.cn/rss/scroll-news.xml")
    else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for item in body.split("<item>").skip(1) {
        let Some(title) = rss_text(item, "title") else {
            continue;
        };
        let link = rss_text(item, "link").unwrap_or_default();
        let when = rss_text(item, "pubDate").unwrap_or_default();
        let n = items.len();
        items.push(json!({
            "id": format!("news-{n}"),
            "title": title,
            "label": format!("要闻  {title}"),
            "source": "中国新闻网",
            "url": link,
            "note": when,
            "rows": [ticker_row(&title, &when, "", &link)],
        }));
        if items.len() == 8 {
            break;
        }
    }
    items
}

fn weather_phrase(code: i64) -> &'static str {
    match code {
        0 => "晴",
        1 | 2 => "少云",
        3 => "阴",
        45 | 48 => "雾",
        51 | 53 | 55 | 56 | 57 | 61 | 63 | 65 | 66 | 67 | 80 | 81 | 82 => "雨",
        71 | 73 | 75 | 77 | 85 | 86 => "雪",
        95 | 96 | 99 => "雷雨",
        _ => "",
    }
}

fn ticker_weather() -> Option<Value> {
    let url = "https://api.open-meteo.com/v1/forecast?latitude=39.90,31.23,35.68,51.51,40.71,1.35&longitude=116.40,121.47,139.69,-0.13,-74.01,103.82&current=temperature_2m,weather_code,wind_speed_10m";
    let v = https_get_json(url).ok()?;
    let names = ["北京", "上海", "东京", "伦敦", "纽约", "新加坡"];
    let spots: Vec<&Value> = if let Some(arr) = v.as_array() {
        arr.iter().collect()
    } else {
        vec![&v]
    };
    let mut rows = Vec::new();
    let mut bits = Vec::new();
    for (name, spot) in names.iter().zip(spots.iter()) {
        let Some(temp) = spot
            .pointer("/current/temperature_2m")
            .and_then(|t| t.as_f64())
        else {
            continue;
        };
        let phrase = spot
            .pointer("/current/weather_code")
            .and_then(|c| c.as_i64())
            .map(weather_phrase)
            .unwrap_or("");
        let wind = spot
            .pointer("/current/wind_speed_10m")
            .and_then(|w| w.as_f64());
        let mut hint = phrase.to_string();
        if let Some(w) = wind {
            if !hint.is_empty() {
                hint.push_str("  ");
            }
            hint.push_str(&format!("风 {w:.0} km/h"));
        }
        bits.push(format!("{name} {temp:.0}°"));
        rows.push(ticker_row(name, &format!("{temp:.0}°"), &hint, ""));
    }
    if rows.is_empty() {
        return None;
    }
    Some(json!({
        "id": "weather",
        "title": "天气",
        "label": format!("天气  {}", bits.join("  ")),
        "source": "Open-Meteo",
        "url": "https://open-meteo.com/",
        "note": "当前气温",
        "rows": rows,
    }))
}

/// 用系统浏览器打开 https 链接。中国新闻网的要闻链接仍是 http，只放行这个域名。
#[tauri::command]
pub(crate) fn open_https(url: String) -> Result<(), String> {
    let url = url.trim();
    let ok = url.starts_with("https://")
        || url.starts_with("http://www.chinanews.com.cn/")
        || url.starts_with("http://www.chinanews.com/");
    if !ok || url.chars().any(|c| c.is_control() || c == ' ') {
        return Err("这个地址不能打开".into());
    }
    let status = {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open").arg(url).status()
        }
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open").arg(url).status()
        }
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .status()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Result::<std::process::ExitStatus, std::io::Error>::Err(std::io::Error::other(
                "当前系统不能打开链接",
            ))
        }
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("系统没有打开这个链接".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// 标题栏跑马灯。只返回勾选的来源；某一路失败就略过，不编造数字。
/// 每一条自带来源地址，点开后由界面放到下方标签页。
#[tauri::command]
pub(crate) async fn ticker_feed(
    fx: bool,
    crypto: bool,
    news: bool,
    weather: bool,
) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let mut items = Vec::new();
        if fx {
            if let Some(v) = ticker_fx() {
                items.push(v);
            }
        }
        if crypto {
            if let Some(v) = ticker_crypto() {
                items.push(v);
            }
        }
        if news {
            items.extend(ticker_news());
        }
        if weather {
            if let Some(v) = ticker_weather() {
                items.push(v);
            }
        }
        json!({ "items": items })
    })
    .await
    .map_err(|e| e.to_string())
}
