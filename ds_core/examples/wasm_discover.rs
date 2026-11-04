//! WASM URL 自动发现独立探测
//!
//! 从 `chat.deepseek.com` 扫出官方 web client 当前使用的
//! `sha3_wasm_bg.<hash>.wasm` 并打印，不走账号、不触碰 PoW。
//! 用于：
//! - 校验 config.toml 里的 `wasm_url` 是否还是最新版本
//! - DeepSeek 更新 WASM 后，一键拿到新 URL 粘回配置
//!
//! 用法：`cargo run -p ds_core --example wasm_discover`
//!   带代理：`DS_WASM_PROBE_PROXY=http://127.0.0.1:7890 cargo run -p ds_core --example wasm_discover`

use std::time::Duration;

use ds_core::{ClientIdentity, DsClient, EmulationProfile, HifConfig, discover_wasm_url};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let proxy = std::env::var("DS_WASM_PROBE_PROXY").ok();

    let identity = ClientIdentity {
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                     AppleWebKit/537.36 (KHTML, like Gecko) \
                     Chrome/140.0.0.0 Safari/537.36"
            .to_string(),
        client_version: "2.5.0".to_string(),
        client_platform: "web".to_string(),
        client_locale: "zh_CN".to_string(),
        client_bundle_id: "com.deepseek.chat".to_string(),
        device_id: String::new(),
        device_model: String::new(),
        timezone_offset: "28800".to_string(),
        client_os: "web".to_string(),
    };

    let client = DsClient::new(
        "https://chat.deepseek.com/api/v0".to_string(),
        "https://fe-static.deepseek.com/chat/static/sha3_wasm_bg.7b9ca65ddd.wasm".to_string(),
        identity,
        &HifConfig {
            enabled: false,
            ..HifConfig::default()
        },
        EmulationProfile::from_config("chrome136").unwrap_or_default(),
        proxy.as_deref(),
    );

    println!("扫描 chat.deepseek.com …");
    let fut = discover_wasm_url(&client);
    match tokio::time::timeout(Duration::from_secs(30), fut).await {
        Ok(Ok(url)) => {
            println!("✓ 发现 WASM URL:\n  {url}");
            println!("\n可将配置 config.toml 的 [ds_core].wasm_url 替换为上面这一行。");
        }
        Ok(Err(e)) => {
            eprintln!("✗ 发现失败: {e}");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("✗ 超时（30s）—— 检查网络 / 代理 / WAF");
            std::process::exit(1);
        }
    }
}
