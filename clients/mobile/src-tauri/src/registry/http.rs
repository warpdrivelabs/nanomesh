use serde_json::Value;

pub(crate) fn registry_base(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() {
        "https://robot.link".into()
    } else {
        s.to_string()
    }
}

fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.eq_ignore_ascii_case(name) {
            Some(v.trim())
        } else {
            None
        }
    })
}

fn decode_chunked(mut rest: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let nl = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("分块不完整")?;
        let size_line = std::str::from_utf8(&rest[..nl]).map_err(|_| "分块长度无效".to_string())?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| "分块长度无效".to_string())?;
        rest = &rest[nl + 2..];
        if size == 0 {
            break;
        }
        if rest.len() < size + 2 {
            return Err("分块被截断".into());
        }
        out.extend_from_slice(&rest[..size]);
        if &rest[size..size + 2] != b"\r\n" {
            return Err("分块格式不对".into());
        }
        rest = &rest[size + 2..];
    }
    Ok(out)
}

pub(crate) fn http_message(raw: &[u8]) -> Result<(u16, String), String> {
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("响应不完整")?;
    let head = std::str::from_utf8(&raw[..sep]).map_err(|_| "响应头不是文本".to_string())?;
    let rest = &raw[sep + 4..];
    let code: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = if header_value(head, "transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        decode_chunked(rest)?
    } else if let Some(n) =
        header_value(head, "content-length").and_then(|v| v.parse::<usize>().ok())
    {
        let n = n.min(rest.len());
        rest[..n].to_vec()
    } else {
        rest.to_vec()
    };
    Ok((code, String::from_utf8_lossy(&body).into_owned()))
}

#[cfg(not(target_os = "ios"))]
fn load_system_certs(builder: &mut openssl::ssl::SslConnectorBuilder) -> Result<(), String> {
    if std::path::Path::new("/etc/ssl/cert.pem").exists() {
        builder
            .set_ca_file("/etc/ssl/cert.pem")
            .map_err(|e| format!("无法加载系统证书: {e}"))?;
        return Ok(());
    }
    let dir = "/system/etc/security/cacerts";
    if std::path::Path::new(dir).is_dir() {
        let mut store = openssl::x509::store::X509StoreBuilder::new()
            .map_err(|e| format!("无法加载系统证书: {e}"))?;
        let mut loaded = 0usize;
        if let Ok(rd) = std::fs::read_dir(dir) {
            for ent in rd.flatten() {
                let Ok(bytes) = std::fs::read(ent.path()) else {
                    continue;
                };
                let cert = openssl::x509::X509::from_pem(&bytes)
                    .or_else(|_| openssl::x509::X509::from_der(&bytes));
                if let Ok(cert) = cert {
                    if store.add_cert(cert).is_ok() {
                        loaded += 1;
                    }
                }
            }
        }
        if loaded == 0 {
            return Err("系统证书目录是空的".into());
        }
        builder.set_cert_store(store.build());
    }
    Ok(())
}

/// 用 OpenSSL 发 HTTPS GET。系统 TLS 和 rustls 连 robot.link 会在 ClientHello 后被重置。
#[cfg(not(target_os = "ios"))]
pub(crate) fn https_get_json(url: &str) -> Result<Value, String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or("注册中心地址必须是 https")?;
    let (hostport, pathq) = rest.split_once('/').unwrap_or((rest, ""));
    let path = format!("/{pathq}");
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| "注册中心端口无效".to_string())?,
        ),
        None => (hostport, 443u16),
    };
    if host.is_empty() {
        return Err("注册中心地址无效".into());
    }
    let tcp =
        std::net::TcpStream::connect((host, port)).map_err(|e| format!("连接注册中心失败: {e}"))?;
    let _ = tcp.set_read_timeout(Some(std::time::Duration::from_secs(20)));
    let _ = tcp.set_write_timeout(Some(std::time::Duration::from_secs(20)));
    let mut builder = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls())
        .map_err(|e| format!("TLS 初始化失败: {e}"))?;
    load_system_certs(&mut builder)?;
    let connector = builder.build();
    let mut stream = connector
        .connect(host, tcp)
        .map_err(|e| format!("注册中心 TLS 失败: {e}"))?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: application/json\r\nAccept-Encoding: identity\r\n\r\n"
    );
    std::io::Write::write_all(&mut stream, req.as_bytes())
        .map_err(|e| format!("请求注册中心失败: {e}"))?;
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut raw)
        .map_err(|e| format!("读取注册中心响应失败: {e}"))?;
    let (code, body) = http_message(&raw)?;
    let v: Value =
        serde_json::from_str(body.trim()).map_err(|e| format!("注册中心响应无法解析: {e}"))?;
    if !(200..300).contains(&code) {
        let err = v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("注册中心拒绝了请求");
        return Err(err.to_string());
    }
    Ok(v)
}

#[cfg(target_os = "ios")]
pub(crate) fn https_get_json(_url: &str) -> Result<Value, String> {
    Err("iOS 暂未接入注册中心 HTTPS".into())
}

pub(crate) async fn http_get_json(url: &str) -> Result<Value, String> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || https_get_json(&url))
        .await
        .map_err(|e| e.to_string())?
}

pub(crate) fn query_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn registry_phrase(err: &str) -> String {
    if err.contains("域名未登记") {
        "这个域名还没有在注册中心登记，不能用来注册或登录。".into()
    } else if err.contains("域名已停用") {
        "这个域名已停用。".into()
    } else if err == "domain_not_owned" {
        "域名已登记，Home Node 尚未同步，请稍后再试。".into()
    } else {
        err.to_string()
    }
}

pub(crate) async fn resolve_home_node(registry: &str, domain: &str) -> Result<String, String> {
    let url = format!(
        "{}/api/resolve?domain={}",
        registry_base(registry),
        query_escape(domain)
    );
    let v = http_get_json(&url).await.map_err(|e| registry_phrase(&e))?;
    v.get("pubkey")
        .and_then(|x| x.as_str())
        .filter(|s| s.len() == 64)
        .map(|s| s.to_string())
        .ok_or_else(|| "注册中心没有返回该域名的节点公钥".into())
}

#[cfg(target_os = "ios")]
pub(crate) fn https_get_raw(_url: &str) -> Result<(u16, String), String> {
    Err("iOS 暂未接入 HTTPS".into())
}

#[cfg(not(target_os = "ios"))]
pub(crate) fn https_get_raw(url: &str) -> Result<(u16, String), String> {
    let rest = url.strip_prefix("https://").ok_or("地址必须是 https")?;
    let (hostport, pathq) = rest.split_once('/').unwrap_or((rest, ""));
    let path = format!("/{pathq}");
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "端口无效".to_string())?),
        None => (hostport, 443u16),
    };
    let tcp = std::net::TcpStream::connect((host, port)).map_err(|e| e.to_string())?;
    let _ = tcp.set_read_timeout(Some(std::time::Duration::from_secs(12)));
    let _ = tcp.set_write_timeout(Some(std::time::Duration::from_secs(12)));
    let mut builder = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls())
        .map_err(|e| e.to_string())?;
    load_system_certs(&mut builder)?;
    let mut stream = builder
        .build()
        .connect(host, tcp)
        .map_err(|e| e.to_string())?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: */*\r\nAccept-Encoding: identity\r\n\r\n"
    );
    std::io::Write::write_all(&mut stream, req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut raw).map_err(|e| e.to_string())?;
    let (code, body) = http_message(&raw)?;
    if !(200..300).contains(&code) {
        return Err(format!("HTTP {code}"));
    }
    Ok((code, body))
}

#[cfg(test)]
mod http_body_tests {
    use super::http_message;

    #[test]
    fn chunked_body_joins_pieces() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let (code, body) = http_message(raw).unwrap();
        assert_eq!(code, 200);
        assert_eq!(body, "{\"a\":1}");
    }

    #[test]
    fn content_length_stops_at_the_declared_end() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":1}TAIL";
        let (code, body) = http_message(raw).unwrap();
        assert_eq!(code, 200);
        assert_eq!(body, "{\"a\":1}");
    }
}
