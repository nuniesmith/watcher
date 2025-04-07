use anyhow::{Context, Result};
use chrono::Local;
use env_logger::{Builder, Env};
use log::{LevelFilter, Record, Level};
use std::io::Write;
use std::sync::Arc;
use tokio::time::Duration;
use std::collections::HashMap;

// Log target for service-specific logs
pub const SERVICE_LOG_TARGET: &str = "service";

// Initialize logger with customizable options
pub fn init(verbose: bool, log_file: Option<&str>) -> Result<()> {
    let env = Env::default()
        .filter_or("RUST_LOG", if verbose { "debug" } else { "info" });
    
    let mut builder = Builder::from_env(env);
    
    // Custom format that includes timestamp, log level, and optional service name
    builder.format(|buf, record| {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
        
        // Extract service name if available
        let service_info = if record.target() == SERVICE_LOG_TARGET {
            if let Some(service) = record.key_values().get("service".into()) {
                format!("[{}] ", service)
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        
        // Colorize output based on level
        let level_str = match record.level() {
            Level::Error => format!("\x1B[31m{}\x1B[0m", record.level()), // Red
            Level::Warn => format!("\x1B[33m{}\x1B[0m", record.level()),  // Yellow
            Level::Info => format!("\x1B[32m{}\x1B[0m", record.level()),  // Green
            Level::Debug => format!("\x1B[36m{}\x1B[0m", record.level()), // Cyan
            Level::Trace => format!("\x1B[35m{}\x1B[0m", record.level()), // Magenta
        };
        
        writeln!(
            buf,
            "[{}] [{}] {}{}",
            timestamp,
            level_str,
            service_info,
            record.args()
        )
    });
    
    // If a log file is specified, also log to file
    if let Some(path) = log_file {
        use std::fs::OpenOptions;
        
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .context(format!("Failed to open log file: {}", path))?;
        
        builder.target(env_logger::Target::Pipe(Box::new(file)));
    }
    
    builder.init();
    
    Ok(())
}

// Logger struct for service-specific logging
#[derive(Clone)]
pub struct ServiceLogger {
    pub service_name: String,
}

impl ServiceLogger {
    pub fn new(service_name: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
        }
    }
    
    pub fn info(&self, message: &str) {
        log::info!(target: SERVICE_LOG_TARGET, service = self.service_name; "{}", message);
    }
    
    pub fn warn(&self, message: &str) {
        log::warn!(target: SERVICE_LOG_TARGET, service = self.service_name; "{}", message);
    }
    
    pub fn error(&self, message: &str) {
        log::error!(target: SERVICE_LOG_TARGET, service = self.service_name; "{}", message);
    }
    
    pub fn debug(&self, message: &str) {
        log::debug!(target: SERVICE_LOG_TARGET, service = self.service_name; "{}", message);
    }
    
    pub fn trace(&self, message: &str) {
        log::trace!(target: SERVICE_LOG_TARGET, service = self.service_name; "{}", message);
    }
}

// Healthcheck client for notifying multiple healthcheck endpoints
pub struct HealthcheckClient {
    client: reqwest::Client,
    endpoints: HashMap<String, String>,
    timeout: Duration,
}

impl HealthcheckClient {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoints: HashMap::new(),
            timeout: Duration::from_secs(timeout_secs),
        }
    }
    
    pub fn add_service(&mut self, service_name: &str, url: &str) {
        if !url.is_empty() {
            self.endpoints.insert(service_name.to_string(), url.to_string());
        }
    }
    
    pub async fn notify(&self, service_name: &str, message: &str, is_error: bool) -> Result<()> {
        if let Some(url) = self.endpoints.get(service_name) {
            if url.is_empty() {
                return Ok(());
            }
            
            let endpoint = if is_error {
                format!("{}/fail", url)
            } else {
                url.to_string()
            };
            
            self.client.post(&endpoint)
                .body(message.to_string())
                .timeout(self.timeout)
                .send()
                .await
                .context(format!("Failed to notify healthcheck for service {}", service_name))?;
        }
        
        Ok(())
    }
    
    pub async fn notify_all(&self, message: &str, is_error: bool) -> Result<()> {
        for (service_name, url) in &self.endpoints {
            if url.is_empty() {
                continue;
            }
            
            let endpoint = if is_error {
                format!("{}/fail", url)
            } else {
                url.to_string()
            };
            
            // Use try_join_all or similar to do these concurrently if needed
            match self.client.post(&endpoint)
                .body(message.to_string())
                .timeout(self.timeout)
                .send()
                .await {
                    Ok(_) => log::debug!("Successfully notified healthcheck for {}", service_name),
                    Err(e) => log::warn!("Failed to notify healthcheck for {}: {}", service_name, e),
                }
        }
        
        Ok(())
    }
}

// Simple function for backward compatibility
pub async fn notify_healthcheck(url: &str, message: &str, is_error: bool) -> Result<()> {
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
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("Failed to notify healthcheck")?;
    
    Ok(())
}