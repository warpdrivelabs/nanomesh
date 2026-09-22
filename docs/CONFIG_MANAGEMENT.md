# Nanomesh 配置管理系统

## 📋 概述

Nanomesh 配置管理系统是一个灵活、类型安全的配置管理解决方案，支持多种即时消息服务器配置、存储服务配置、Web管理配置等，并提供开放的扩展机制。

## 🚀 特性

- ✅ **多IM服务器支持**: 配置多个即时消息服务器实例
- ✅ **负载均衡**: 支持轮询、权重、最少连接等策略
- ✅ **动态配置**: 运行时动态添加/删除服务器实例
- ✅ **类型安全**: 基于Rust类型系统的配置验证
- ✅ **多格式支持**: 支持TOML和JSON配置文件
- ✅ **扩展机制**: 开放的自定义配置扩展
- ✅ **配置验证**: 自动验证配置的有效性
- ✅ **热重载**: 支持配置文件热重载

## 📁 配置结构

### 主要配置部分

```toml
[im_servers]           # IM服务器集群配置
[storage]              # 存储服务配置
[web_server]           # Web管理服务器配置
[network]              # 网络传输配置
[logging]              # 日志配置
[security]             # 安全配置
[monitoring]           # 监控配置
[extensions]           # 自定义扩展配置
```

### IM服务器配置

```toml
[im_servers]
load_balance_strategy = "round_robin"  # 负载均衡策略
health_check_interval = 30             # 健康检查间隔（秒）
failover_enabled = true                # 故障转移

[[im_servers.instances]]
name = "im-server-1"
server_id = 1001
address = "127.0.0.1"
port = 9001
max_connections = 10000
message_queue_size = 1000
worker_threads = 4
max_messages_per_second = 1000
enabled = true
weight = 100
```

### 存储服务配置

```toml
[storage]
storage_type = "postgresql"
address = "127.0.0.1"
port = 5432
database = "nanomesh"
username = "nanomesh_user"
password = "your_secure_password"
max_connections = 100
connection_timeout = 30
ssl_enabled = true

[storage.pool_config]
min_connections = 5
max_connections = 100
idle_timeout = 600
max_lifetime = 3600
```

### Web管理服务器配置

```toml
[web_server]
address = "0.0.0.0"
port = 8080
enable_https = false
cert_path = "./certs/cert.pem"
key_path = "./certs/key.pem"
static_dir = "./static"
max_request_size = 10485760  # 10MB
request_timeout = 30

[web_server.cors]
enabled = true
allowed_origins = ["*"]
allowed_methods = ["GET", "POST", "PUT", "DELETE"]
allowed_headers = ["Content-Type", "Authorization"]
```

## 🛠️ 使用方法

### 1. 基本使用

```rust
use atombase::config::*;

// 初始化配置
init_config_from_toml("./config.toml")?;

// 获取配置
let config = get_config()?;
println!("Web服务器端口: {}", config.web_server.port);

// 获取启用的IM服务器
let servers = get_enabled_im_servers()?;
for server in servers {
    println!("服务器: {}:{}", server.address, server.port);
}
```

### 2. 动态配置管理

```rust
// 添加新的IM服务器
let new_server = ImServerInstance {
    name: "new-server".to_string(),
    server_id: 2001,
    address: "192.168.1.100".to_string(),
    port: 9004,
    max_connections: 8000,
    message_queue_size: 800,
    worker_threads: 3,
    max_messages_per_second: 800,
    enabled: true,
    weight: 80,
};

update_config(|config| {
    config.add_im_server(new_server)?;
    Ok(())
})?;

// 修改配置
update_config(|config| {
    config.web_server.port = 8888;
})?;
```

### 3. 自定义扩展配置

```rust
use serde_json::json;

// 设置自定义配置
update_config(|config| {
    let redis_config = json!({
        "enabled": true,
        "host": "127.0.0.1",
        "port": 6379,
        "database": 0
    });
    config.set_extension("redis", redis_config)?;
    Ok(())
})?;

// 读取自定义配置
with_config(|config| {
    if let Ok(redis_config) = config.get_extension::<serde_json::Value>("redis") {
        println!("Redis配置: {}", redis_config);
    }
})?;
```

### 4. 配置验证

```rust
// 验证配置
with_config(|config| {
    match config.validate() {
        Ok(()) => println!("配置有效"),
        Err(e) => println!("配置错误: {}", e),
    }
})?;
```

## 📝 配置文件示例

完整的配置文件示例请参考 `nanomesh_config.toml`。

## 🔧 API 参考

### 初始化函数

- `init_config_from_toml(path)` - 从TOML文件初始化配置
- `init_config_from_json(path)` - 从JSON文件初始化配置
- `init_default_config()` - 初始化默认配置

### 配置访问函数

- `get_config()` - 获取完整配置的克隆
- `with_config(f)` - 使用只读配置执行函数
- `update_config(f)` - 更新配置

### 便捷访问函数

- `get_enabled_im_servers()` - 获取启用的IM服务器列表
- `get_web_server_config()` - 获取Web服务器配置
- `get_storage_config()` - 获取存储配置
- `get_network_config()` - 获取网络配置
- `get_log_config()` - 获取日志配置
- `get_security_config()` - 获取安全配置
- `get_monitoring_config()` - 获取监控配置

### 文件操作函数

- `save_config_to_toml(path)` - 保存配置到TOML文件
- `save_config_to_json(path)` - 保存配置到JSON文件
- `create_default_toml_config(path)` - 创建默认TOML配置文件
- `create_default_json_config(path)` - 创建默认JSON配置文件

## 🎯 最佳实践

### 1. 环境配置

为不同环境创建不同的配置文件：

```
config/
├── development.toml
├── testing.toml
└── production.toml
```

### 2. 配置验证

在应用启动时验证配置：

```rust
fn main() -> Result<(), Box<dyn Error>> {
    init_config_from_toml("./config.toml")?;
    
    // 验证配置
    with_config(|config| {
        config.validate()
    })??;
    
    // 启动应用...
    Ok(())
}
```

### 3. 敏感信息

敏感信息（如密码）应通过环境变量设置：

```rust
use std::env;

update_config(|config| {
    if let Ok(password) = env::var("DB_PASSWORD") {
        config.storage.password = Some(password);
    }
})?;
```

### 4. 配置热重载

```rust
// 监听配置文件变化并重新加载
fn reload_config_if_changed() -> ConfigResult<()> {
    reload_config_from_toml("./config.toml")?;
    println!("配置已重新加载");
    Ok(())
}
```

## 🚨 注意事项

1. **线程安全**: 配置系统使用RwLock，支持多线程安全访问
2. **性能考虑**: 频繁的配置更新可能影响性能，建议批量更新
3. **配置验证**: 始终在加载配置后进行验证
4. **备份配置**: 在生产环境中定期备份配置文件

## 🔍 故障排除

### 常见错误

1. **配置文件不存在**: 确保配置文件路径正确
2. **配置格式错误**: 检查TOML/JSON语法
3. **配置验证失败**: 检查必填字段和数值范围
4. **权限问题**: 确保应用有读写配置文件的权限

### 调试技巧

```rust
// 打印当前配置
with_config(|config| {
    println!("当前配置: {:#?}", config);
})?;

// 验证特定配置部分
with_config(|config| {
    for server in &config.im_servers.instances {
        if server.port == 0 {
            println!("警告: 服务器 {} 端口为0", server.name);
        }
    }
})?;
```

## 📚 更多示例

运行演示程序查看更多使用示例：

```bash
cargo run --bin config_demo
```

这将展示配置系统的各种功能和用法。
