use anyhow::{Context, Result};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Service type enumeration for specialized handling
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Nginx,
    Apache,
    Generic,
    Custom(String),
}

/// Permissions configuration for file ownership
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permissions {
    pub fix: bool,
    pub user: String,
    pub group: String,
}

/// Individual service configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    // Service identification
    pub name: String,
    pub container_name: String,
    #[serde(default = "default_service_type")]
    pub service_type: ServiceType,
    
    // Repository settings
    pub repo_url: String,
    pub branch: Option<String>,
    pub local_path: PathBuf,
    
    // Container settings
    #[serde(default)]
    pub use_docker_compose: bool,
    pub docker_compose_file: Option<String>,
    #[serde(default)]
    pub docker_compose_dir: Option<PathBuf>,
    pub restart_command: Option<String>,
    pub validation_command: Option<String>,
    
    // Behavior settings
    #[serde(default)]
    pub disable_restart: bool,
    pub healthcheck_url: Option<String>,
    pub auto_fix: Option<bool>,
    pub monitor_logs: Option<bool>,
    #[serde(default = "default_log_tail_lines")]
    pub log_tail_lines: u32,
    
    // Permissions
    pub permissions: Option<Permissions>,
    
    // Service-specific settings
    #[serde(flatten)]
    pub custom_settings: HashMap<String, serde_json::Value>,
}

/// Global settings for application behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSettings {
    #[serde(default = "default_watch_interval")]
    pub watch_interval: u64,
    #[serde(default)]
    pub default_branch: String,
    #[serde(default)]
    pub auto_fix: bool,
    #[serde(default = "default_true")]
    pub fix_permissions: bool,
    #[serde(default = "default_true")]
    pub monitor_logs: bool,
    #[serde(default)]
    pub disable_restart: bool,
    #[serde(default)]
    pub use_docker_compose: bool,
    #[serde(default)]
    pub default_compose_dir: Option<PathBuf>,
    #[serde(default)]
    pub default_compose_file: Option<String>,
    #[serde(default = "default_startup_grace_period")]
    pub startup_grace_period: String,
}

/// Main configuration containing all services and global settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub services: Vec<ServiceConfig>,
    #[serde(default)]
    pub global_settings: GlobalSettings,
}

/// Legacy configuration for backward compatibility
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyConfig {
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

// Default function implementations
fn default_service_type() -> ServiceType {
    ServiceType::Generic
}

fn default_watch_interval() -> u64 {
    60
}

fn default_log_tail_lines() -> u32 {
    100
}

fn default_true() -> bool {
    true
}

fn default_startup_grace_period() -> String {
    "30s".to_string()
}

// Implementation blocks for the structs

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            watch_interval: default_watch_interval(),
            default_branch: "main".to_string(),
            auto_fix: false,
            fix_permissions: default_true(),
            monitor_logs: default_true(),
            disable_restart: false,
            use_docker_compose: false,
            default_compose_dir: Some(PathBuf::from("/app/config")),
            default_compose_file: Some("docker-compose.yml".to_string()),
            startup_grace_period: default_startup_grace_period(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            global_settings: GlobalSettings::default(),
            services: vec![ServiceConfig::default_nginx()],
        }
    }
}

impl ServiceConfig {
    /// Create a default Nginx service config
    pub fn default_nginx() -> Self {
        let config_dir = PathBuf::from("/app/config/nginx");
        
        Self {
            name: "nginx".to_string(),
            container_name: "nginx_app".to_string(),
            service_type: ServiceType::Nginx,
            
            repo_url: "https://github.com/nuniesmith/nginx.git".to_string(),
            branch: Some("main".to_string()),
            local_path: config_dir.clone(),
            
            use_docker_compose: false,
            docker_compose_file: None,
            docker_compose_dir: None,
            restart_command: Some("docker restart nginx_app".to_string()),
            validation_command: Some("docker exec -t nginx_app nginx -t".to_string()),
            
            disable_restart: false,
            healthcheck_url: None,
            auto_fix: None,
            monitor_logs: Some(true),
            log_tail_lines: default_log_tail_lines(),
            
            permissions: Some(Permissions {
                fix: true,
                user: "nginx".to_string(),
                group: "nginx".to_string(),
            }),
            
            custom_settings: HashMap::new(),
        }
    }
    
    /// Get the effective branch (considers the default)
    pub fn effective_branch(&self, default: &str) -> String {
        self.branch.clone().unwrap_or_else(|| default.to_string())
    }
    
    /// Get the effective auto_fix (considers the default)
    pub fn effective_auto_fix(&self, default: bool) -> bool {
        self.auto_fix.unwrap_or(default)
    }
    
    /// Get the effective monitor_logs (considers the default)
    pub fn effective_monitor_logs(&self, default: bool) -> bool {
        self.monitor_logs.unwrap_or(default)
    }
    
    /// Get the effective fix_permissions (considers the default)
    pub fn effective_fix_permissions(&self, default: bool) -> bool {
        self.permissions.as_ref().map_or(default, |p| p.fix)
    }
    
    /// Get docker compose directory, falling back to the default if not set
    pub fn get_compose_dir(&self, default_dir: &Option<PathBuf>) -> Option<PathBuf> {
        self.docker_compose_dir.clone().or_else(|| default_dir.clone())
    }
    
    /// Get docker compose file, falling back to the default if not set
    pub fn get_compose_file(&self, default_file: &Option<String>) -> Option<String> {
        self.docker_compose_file.clone().or_else(|| default_file.clone())
    }
}

impl Default for LegacyConfig {
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

// Convert legacy config to new format
impl From<&LegacyConfig> for Config {
    fn from(legacy: &LegacyConfig) -> Self {
        let service = ServiceConfig {
            name: "nginx".to_string(),
            container_name: legacy.nginx_container_name.clone(),
            service_type: ServiceType::Nginx,
            
            repo_url: legacy.repo_url.clone(),
            branch: Some(legacy.branch.clone()),
            local_path: legacy.config_dir.clone(),
            
            use_docker_compose: legacy.use_docker_compose,
            docker_compose_file: Some(legacy.compose_file.clone()),
            docker_compose_dir: Some(legacy.compose_dir.clone()),
            restart_command: Some(format!("docker restart {}", legacy.nginx_container_name)),
            validation_command: Some(format!("docker exec -t {} nginx -t", legacy.nginx_container_name)),
            
            disable_restart: legacy.disable_restart,
            healthcheck_url: legacy.healthcheck_url.clone(),
            auto_fix: Some(legacy.auto_fix),
            monitor_logs: Some(legacy.monitor_logs),
            log_tail_lines: legacy.log_tail_lines,
            
            permissions: Some(Permissions {
                fix: legacy.fix_permissions,
                user: legacy.nginx_user.clone(),
                group: legacy.nginx_group.clone(),
            }),
            
            custom_settings: {
                let mut map = HashMap::new();
                map.insert("web_root".to_string(), serde_json::Value::String(legacy.web_root.clone()));
                map.insert("enable_dir_listing".to_string(), serde_json::Value::Bool(legacy.enable_dir_listing));
                map
            },
        };
        
        let global = GlobalSettings {
            watch_interval: legacy.watch_interval,
            default_branch: legacy.branch.clone(),
            auto_fix: legacy.auto_fix,
            fix_permissions: legacy.fix_permissions,
            monitor_logs: legacy.monitor_logs,
            disable_restart: legacy.disable_restart,
            use_docker_compose: legacy.use_docker_compose,
            default_compose_dir: Some(legacy.compose_dir.clone()),
            default_compose_file: Some(legacy.compose_file.clone()),
            startup_grace_period: "30s".to_string(),
        };
        
        Self {
            global_settings: global,
            services: vec![service],
        }
    }
}

/// Load configurations from various sources (JSON, environment variables)
impl Config {
    /// Load configuration, trying multi-service JSON first, then falling back to legacy config
    pub fn load() -> Result<Self> {
        // First, check if SERVICES_CONFIG env var is set and points to a valid file
        if let Ok(services_config_path) = env::var("SERVICES_CONFIG") {
            let path = Path::new(&services_config_path);
            if path.exists() {
                info!("Loading multi-service configuration from {}", path.display());
                return Self::load_from_json(path);
            } else {
                warn!("Services config file {} not found, falling back to legacy config", path.display());
            }
        }
        
        // If no services.json, fall back to legacy config
        info!("Loading legacy configuration from environment variables");
        let legacy_config = Self::load_legacy_from_env()?;
        
        // Convert legacy config to new format
        Ok(Config::from(&legacy_config))
    }
    
    /// Load multi-service config from a JSON file
    pub fn load_from_json(path: &Path) -> Result<Self> {
        let file_content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read services config file: {}", path.display()))?;
            
        let config: Config = serde_json::from_str(&file_content)
            .with_context(|| format!("Failed to parse services config file: {}", path.display()))?;
            
        // Validate at least one service exists
        if config.services.is_empty() {
            warn!("No services defined in config file. Adding default nginx service.");
            let mut config = config;
            config.services.push(ServiceConfig::default_nginx());
            return Ok(config);
        }
        
        Ok(config)
    }
    
    /// Load legacy config from environment variables
    pub fn load_legacy_from_env() -> Result<LegacyConfig> {
        let mut config = LegacyConfig::default();
        
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
    
    /// Display configuration in a human-readable format
    pub fn display(&self) {
        info!("== Global Configuration ==");
        info!("Watch Interval: {} seconds", self.global_settings.watch_interval);
        info!("Default Branch: {}", self.global_settings.default_branch);
        info!("Default Auto Fix: {}", self.global_settings.auto_fix);
        info!("Default Fix Permissions: {}", self.global_settings.fix_permissions);
        info!("Default Monitor Logs: {}", self.global_settings.monitor_logs);
        info!("Default Disable Restart: {}", self.global_settings.disable_restart);
        info!("Default Use Docker Compose: {}", self.global_settings.use_docker_compose);
        
        if let Some(dir) = &self.global_settings.default_compose_dir {
            info!("Default Compose Directory: {}", dir.display());
        }
        
        if let Some(file) = &self.global_settings.default_compose_file {
            info!("Default Compose File: {}", file);
        }
        
        info!("Startup Grace Period: {}", self.global_settings.startup_grace_period);
        info!("Number of Services: {}", self.services.len());
        
        for (i, service) in self.services.iter().enumerate() {
            info!("");
            info!("== Service {} - {} ==", i + 1, service.name);
            info!("Container: {}", service.container_name);
            info!("Type: {:?}", service.service_type);
            info!("Repository URL: {}", service.repo_url);
            info!("Branch: {}", service.effective_branch(&self.global_settings.default_branch));
            info!("Config Directory: {}", service.local_path.display());
            
            info!("Docker Compose: {}", service.use_docker_compose || self.global_settings.use_docker_compose);
            
            if let Some(dir) = service.get_compose_dir(&self.global_settings.default_compose_dir) {
                info!("Compose Directory: {}", dir.display());
            }
            
            if let Some(file) = service.get_compose_file(&self.global_settings.default_compose_file) {
                info!("Compose File: {}", file);
            }
            
            if let Some(cmd) = &service.restart_command {
                info!("Restart Command: {}", cmd);
            }
            
            if let Some(cmd) = &service.validation_command {
                info!("Validation Command: {}", cmd);
            }
            
            info!("Disable Restart: {}", service.disable_restart);
            
            if let Some(url) = &service.healthcheck_url {
                info!("Healthcheck URL: {}", url);
            }
            
            info!("Auto Fix: {}", service.effective_auto_fix(self.global_settings.auto_fix));
            info!("Monitor Logs: {}", service.effective_monitor_logs(self.global_settings.monitor_logs));
            info!("Log Tail Lines: {}", service.log_tail_lines);
            info!("Fix Permissions: {}", service.effective_fix_permissions(self.global_settings.fix_permissions));
            
            if let Some(perms) = &service.permissions {
                info!("User/Group: {}:{}", perms.user, perms.group);
            }
            
            // Display custom settings if any
            if !service.custom_settings.is_empty() {
                info!("Custom Settings:");
                for (key, value) in &service.custom_settings {
                    info!("  {}: {}", key, value);
                }
            }
        }
    }
    
    /// Create a simplified Nginx config for the docker module
    pub fn to_nginx_config(&self, service_idx: usize) -> Result<nginx::Config> {
        if service_idx >= self.services.len() {
            return Err(anyhow::anyhow!("Service index out of bounds"));
        }
        
        let service = &self.services[service_idx];
        
        // Only create an nginx config for Nginx service types
        if service.service_type != ServiceType::Nginx {
            return Err(anyhow::anyhow!("Service is not an Nginx service"));
        }
        
        let compose_dir = service.get_compose_dir(&self.global_settings.default_compose_dir)
            .unwrap_or_else(|| PathBuf::from("."));
            
        let compose_file = service.get_compose_file(&self.global_settings.default_compose_file)
            .unwrap_or_else(|| "docker-compose.yml".to_string());
        
        Ok(nginx::Config {
            nginx_container_name: service.container_name.clone(),
            compose_dir,
            compose_file,
            use_docker_compose: service.use_docker_compose || self.global_settings.use_docker_compose,
            disable_restart: service.disable_restart || self.global_settings.disable_restart,
            monitor_logs: service.effective_monitor_logs(self.global_settings.monitor_logs),
            log_tail_lines: service.log_tail_lines,
            force_rebuild: None,
        })
    }
}

// Module declaration for nginx to avoid circular dependencies
pub mod nginx {
    use std::path::PathBuf;
    use serde::{Deserialize, Serialize};
    
    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct Config {
        pub nginx_container_name: String,
        pub compose_dir: PathBuf,
        pub compose_file: String,
        pub use_docker_compose: bool,
        pub disable_restart: bool,
        pub monitor_logs: bool,
        pub log_tail_lines: u32,
        pub force_rebuild: Option<bool>,
    }
}