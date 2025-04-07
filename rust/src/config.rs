use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    // Repository settings
    pub repo_url: String,
    pub branch: String,
    pub watch_interval: u64,
    pub ssh_private_key: Option<String>,
    
    // Docker settings
    pub nginx_container_name: String,
    pub use_docker_compose: bool,
    pub compose_file: String,
    pub compose_dir: PathBuf,
    
    // Path settings
    pub config_dir: PathBuf,
    pub lockfile: PathBuf,
    pub web_root: String,
    
    // Behavior settings
    pub verbose: bool,
    pub disable_restart: bool,
    pub healthcheck_url: Option<String>,
    pub auto_fix: bool,
    pub monitor_logs: bool,
    pub log_tail_lines: u32,
    pub fix_permissions: bool,
    pub enable_dir_listing: bool,
    
    // User settings
    pub nginx_user: String,
    pub nginx_group: String,
}

impl Default for Config {
    fn default() -> Self {
        let config_dir = PathBuf::from("/app/config");
        
        Self {
            repo_url: "https://github.com/nuniesmith/nginx.git".to_string(),
            branch: "main".to_string(),
            watch_interval: 300,
            ssh_private_key: None,
            
            nginx_container_name: "nginx".to_string(),
            use_docker_compose: true,
            compose_file: "docker-compose.yml".to_string(),
            compose_dir: config_dir.clone(),
            
            config_dir,
            lockfile: PathBuf::from("/var/run/nginx_config_watcher.lock"),
            web_root: "/var/www/html".to_string(),
            
            verbose: false,
            disable_restart: false,
            healthcheck_url: None,
            auto_fix: false,
            monitor_logs: true,
            log_tail_lines: 100,
            fix_permissions: true,
            enable_dir_listing: false,
            
            nginx_user: "nginx".to_string(),
            nginx_group: "nginx".to_string(),
        }
    }
}

pub fn load_config() -> Result<Config> {
    let mut config = Config::default();
    
    // Override from environment variables
    if let Ok(value) = env::var("REPO_URL") {
        config.repo_url = value;
    }
    
    if let Ok(value) = env::var("BRANCH") {
        config.branch = value;
    }
    
    if let Ok(value) = env::var("WATCH_INTERVAL") {
        if let Ok(interval) = value.parse::<u64>() {
            config.watch_interval = interval;
        }
    }
    
    if let Ok(value) = env::var("NGINX_CONTAINER_NAME") {
        config.nginx_container_name = value;
    }
    
    if let Ok(value) = env::var("CONFIG_DIR") {
        config.config_dir = PathBuf::from(value);
    }
    
    if let Ok(value) = env::var("LOCKFILE") {
        config.lockfile = PathBuf::from(value);
    }
    
    if let Ok(value) = env::var("USE_DOCKER_COMPOSE") {
        config.use_docker_compose = value.to_lowercase() == "true";
    }
    
    if let Ok(value) = env::var("COMPOSE_FILE") {
        config.compose_file = value;
    }
    
    if let Ok(value) = env::var("COMPOSE_DIR") {
        config.compose_dir = PathBuf::from(value);
    }
    
    if let Ok(value) = env::var("VERBOSE") {
        config.verbose = value.to_lowercase() == "true";
    }
    
    if let Ok(value) = env::var("DISABLE_RESTART") {
        config.disable_restart = value.to_lowercase() == "true";
    }
    
    if let Ok(value) = env::var("HEALTHCHECK_URL") {
        if !value.is_empty() {
            config.healthcheck_url = Some(value);
        }
    }
    
    if let Ok(value) = env::var("AUTO_FIX") {
        config.auto_fix = value.to_lowercase() == "true";
    }
    
    if let Ok(value) = env::var("MONITOR_LOGS") {
        config.monitor_logs = value.to_lowercase() == "true";
    }
    
    if let Ok(value) = env::var("LOG_TAIL_LINES") {
        if let Ok(lines) = value.parse::<u32>() {
            config.log_tail_lines = lines;
        }
    }
    
    if let Ok(value) = env::var("FIX_PERMISSIONS") {
        config.fix_permissions = value.to_lowercase() == "true";
    }
    
    if let Ok(value) = env::var("NGINX_USER") {
        config.nginx_user = value;
    }
    
    if let Ok(value) = env::var("NGINX_GROUP") {
        config.nginx_group = value;
    }
    
    if let Ok(value) = env::var("WEB_ROOT") {
        config.web_root = value;
    }
    
    if let Ok(value) = env::var("ENABLE_DIR_LISTING") {
        config.enable_dir_listing = value.to_lowercase() == "true";
    }
    
    if let Ok(value) = env::var("SSH_PRIVATE_KEY") {
        if !value.is_empty() {
            config.ssh_private_key = Some(value);
        }
    }
    
    Ok(config)
}

pub fn show_config(config: &Config) {
    log::info!("Current Configuration:");
    log::info!("REPO_URL: {}", config.repo_url);
    log::info!("BRANCH: {}", config.branch);
    log::info!("WATCH_INTERVAL: {} seconds", config.watch_interval);
    log::info!("NGINX_CONTAINER_NAME: {}", config.nginx_container_name);
    log::info!("CONFIG_DIR: {}", config.config_dir.display());
    log::info!("USE_DOCKER_COMPOSE: {}", config.use_docker_compose);
    log::info!("COMPOSE_FILE: {}", config.compose_file);
    log::info!("COMPOSE_DIR: {}", config.compose_dir.display());
    log::info!("VERBOSE: {}", config.verbose);
    log::info!("DISABLE_RESTART: {}", config.disable_restart);
    log::info!("AUTO_FIX: {}", config.auto_fix);
    log::info!("FIX_PERMISSIONS: {}", config.fix_permissions);
    log::info!("MONITOR_LOGS: {}", config.monitor_logs);
    log::info!("LOG_TAIL_LINES: {}", config.log_tail_lines);
    log::info!("NGINX_USER: {}", config.nginx_user);
    log::info!("NGINX_GROUP: {}", config.nginx_group);
    log::info!("WEB_ROOT: {}", config.web_root);
    log::info!("ENABLE_DIR_LISTING: {}", config.enable_dir_listing);
    
    if let Some(url) = &config.healthcheck_url {
        log::info!("HEALTHCHECK_URL: {}", url);
    } else {
        log::info!("HEALTHCHECK_URL: [not configured]");
    }
    
    if config.ssh_private_key.is_some() {
        log::info!("SSH_PRIVATE_KEY: [configured]");
    } else {
        log::info!("SSH_PRIVATE_KEY: [not configured]");
    }
}