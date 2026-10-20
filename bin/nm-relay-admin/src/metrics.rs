//! 抓取并解析 iroh-relay / iroh-dns-server 的 Prometheus 文本指标。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

pub struct MetricsClient {
    http: reqwest::Client,
    relay_url: String,
    dns_url: String,
    /// 上次采样，用于估算速率。
    prev: Mutex<Option<Sample>>,
}

#[derive(Clone)]
struct Sample {
    at: Instant,
    relay: HashMap<String, f64>,
    dns: HashMap<String, f64>,
}
 
#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub relay_up: bool,
    pub dns_up: bool,
    pub relay: HashMap<String, f64>,
    pub dns: HashMap<String, f64>,
    /// 估算字节/秒（基于两次 scrape 差分）。
    pub rates: HashMap<String, f64>,
    pub scraped_at_ms: u64,
}

impl MetricsClient {
    pub fn new(relay_metrics: &str, dns_metrics: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(2))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            relay_url: relay_metrics.trim_end_matches('/').to_string(),
            dns_url: dns_metrics.trim_end_matches('/').to_string(),
            prev: Mutex::new(None),
        }
    }

    pub async fn scrape(&self) -> Overview {
        let (relay_r, dns_r) = tokio::join!(
            self.fetch_map(&self.relay_url),
            self.fetch_map(&self.dns_url)
        );
        let relay_up = relay_r.is_ok();
        let dns_up = dns_r.is_ok();
        let relay = relay_r.unwrap_or_default();
        let dns = dns_r.unwrap_or_default();
        let now = Instant::now();
        let mut rates = HashMap::new();
        {
            let mut prev = self.prev.lock().unwrap();
            if let Some(p) = prev.as_ref() {
                let dt = now.duration_since(p.at).as_secs_f64().max(0.001);
                for key in [
                    "relayserver_bytes_sent_total",
                    "relayserver_bytes_recv_total",
                    "relayserver_accepts_total",
                    "dns_server_http_requests_total",
                    "dns_server_pkarr_publish_update_total",
                ] {
                    if let (Some(a), Some(b)) = (relay.get(key).or(dns.get(key)), p.relay.get(key).or(p.dns.get(key))) {
                        rates.insert(format!("{key}_per_s"), (a - b).max(0.0) / dt);
                    }
                }
                // 便捷别名
                if let Some(v) = rates.get("relayserver_bytes_sent_total_per_s") {
                    rates.insert("bytes_tx_per_s".into(), *v);
                }
                if let Some(v) = rates.get("relayserver_bytes_recv_total_per_s") {
                    rates.insert("bytes_rx_per_s".into(), *v);
                }
            }
            *prev = Some(Sample {
                at: now,
                relay: relay.clone(),
                dns: dns.clone(),
            });
        }
        Overview {
            relay_up,
            dns_up,
            relay,
            dns,
            rates,
            scraped_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    async fn fetch_map(&self, base: &str) -> Result<HashMap<String, f64>, String> {
        let url = if base.ends_with("/metrics") {
            base.to_string()
        } else {
            format!("{base}/metrics")
        };
        let text = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        Ok(parse_prometheus(&text))
    }
}

/// 极简 Prometheus 文本解析：只取无 label 的 counter/gauge 末尾数值。
fn parse_prometheus(text: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // name{labels} value 或 name value
        let (name, rest) = if let Some(i) = line.find('{') {
            let name = &line[..i];
            let Some(j) = line.find('}') else { continue };
            (name, line[j + 1..].trim())
        } else {
            let mut parts = line.split_whitespace();
            let Some(name) = parts.next() else { continue };
            let Some(val) = parts.next() else { continue };
            if let Ok(v) = val.parse::<f64>() {
                out.insert(name.to_string(), v);
            }
            continue;
        };
        let val = rest.split_whitespace().next().unwrap_or("");
        if let Ok(v) = val.parse::<f64>() {
            // 无 label 的优先；有 label 的用 name 聚合（求和）对计数器够用
            *out.entry(name.to_string()).or_insert(0.0) += v;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let t = r#"
# HELP relayserver_bytes_sent Number of bytes sent.
# TYPE relayserver_bytes_sent counter
relayserver_bytes_sent_total 189349238
dns_server_pkarr_publish_update_total 12
"#;
        let m = parse_prometheus(t);
        assert_eq!(m.get("relayserver_bytes_sent_total"), Some(&189349238.0));
        assert_eq!(m.get("dns_server_pkarr_publish_update_total"), Some(&12.0));
    }
}
