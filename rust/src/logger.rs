use anyhow::Result;
use chrono::Local;
use env_logger::{Builder, Env};
use log::LevelFilter;
use std::io::Write;

pub fn init(verbose: bool) {
    let env = Env::default()
        .filter_or("RUST_LOG", if verbose { "debug" } else { "info" });
    
    Builder::from_env(env)
        .format(|buf, record| {
            writeln!(
                buf,
                "[{}] [{}] {}",
                Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.args()
            )
        })
        .init();
}

pub async fn notify_healthcheck(
    url: &str, 
    message: &str, 
    is_error: bool
) -> Result<()> {
    if url.is_empty() {
        return Ok(());
    }
    
    let client = reqwest::Client::new();
    
    let endpoint = if is_error {
        format!("{}/fail", url)
    } else {
        url.to_string()
    };
    
    client.post(&endpoint)
        .body(message.to_string())
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;
    
    Ok(())
}