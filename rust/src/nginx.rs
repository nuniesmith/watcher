use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};
use regex::Regex;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use walkdir::WalkDir;

use crate::config::{GlobalSettings, Permissions, ServiceConfig, ServiceType, nginx::Config as NginxConfig};
use crate::docker_utils::{
    ContainerStatus, DockerComposeConfig, check_container_status, 
    get_container_logs, recreate_with_docker_compose, restart_container, 
    restart_with_docker_compose
};

/// Check the status of the Nginx container
pub async fn check_nginx_status(config: &NginxConfig) -> Result<ContainerStatus> {
    check_container_status(&config.nginx_container_name).await
}

/// Restart the Nginx container based on configuration
pub async fn restart_nginx(config: &NginxConfig) -> Result<()> {
    if config.disable_restart {
        info!("Container restart is disabled by configuration. Skipping.");
        return Ok(());
    }
    
    info!("Restarting Nginx container: {}", config.nginx_container_name);
    
    if config.use_docker_compose {
        restart_nginx_with_compose(config).await
    } else {
        restart_container(&config.nginx_container_name).await
    }
}

/// Restart Nginx using Docker Compose
async fn restart_nginx_with_compose(config: &NginxConfig) -> Result<()> {
    // Determine if we need a full recreate (down, build, up) or just a restart
    let compose_config = DockerComposeConfig {
        compose_dir: config.compose_dir.clone(),
        compose_file: Some(config.compose_file.clone()),
        service_name: config.nginx_container_name.clone(),
    };
    
    // If force_rebuild is enabled, do a full recreate
    if config.force_rebuild.unwrap_or(false) {
        recreate_with_docker_compose(&compose_config).await
    } else {
        restart_with_docker_compose(&compose_config).await
    }
}

/// Check Nginx logs for errors
pub async fn check_nginx_logs(config: &NginxConfig) -> Result<()> {
    if !config.monitor_logs {
        debug!("Log monitoring is disabled");
        return Ok(());
    }
    
    info!("Checking Nginx logs for errors");
    
    // Check if container is running
    let status = check_container_status(&config.nginx_container_name).await?;
    if status != ContainerStatus::Running {
        warn!("Cannot check logs - Nginx container is not running");
        return Ok(());
    }
    
    // Get logs from the container
    let logs = get_container_logs(&config.nginx_container_name, config.log_tail_lines).await?;
    
    // Check for errors
    let errors: Vec<&str> = logs.lines()
        .filter(|line| line.to_lowercase().contains("error"))
        .collect();
    
    if !errors.is_empty() {
        warn!("Found {} errors in Nginx logs:", errors.len());
        
        // Show the first few errors
        for (i, error) in errors.iter().take(5).enumerate() {
            warn!("[{}] NGINX: {}", i + 1, error);
        }
        
        // Count 403 errors
        let count_403 = errors.iter()
            .filter(|line| line.contains("403"))
            .count();
        
        if count_403 > 0 {
            warn!("Found {} '403 Forbidden' errors - check directory permissions and index files", count_403);
        }
    } else {
        debug!("No errors found in recent Nginx logs");
    }
    
    Ok(())
}

//----------------------------------------
// Extended Nginx Service Implementation
//----------------------------------------

/// Nginx service handler - implements full management capabilities
pub struct NginxService<'a> {
    service: &'a ServiceConfig,
    global: &'a GlobalSettings,
    custom_settings: HashMap<String, String>,
}

impl<'a> NginxService<'a> {
    /// Create a new NginxService instance
    pub fn new(service: &'a ServiceConfig, global: &'a GlobalSettings) -> Result<Self> {
        // Validate service type
        if service.service_type != ServiceType::Nginx {
            return Err(anyhow!("Service type mismatch: expected Nginx service"));
        }
        
        // Extract custom settings
        let mut custom_settings = HashMap::new();
        
        // Extract web_root setting
        if let Some(value) = service.custom_settings.get("web_root") {
            if let Some(web_root) = value.as_str() {
                custom_settings.insert("web_root".to_string(), web_root.to_string());
            }
        } else {
            custom_settings.insert("web_root".to_string(), "/var/www/html".to_string());
        }
        
        // Extract directory listing setting
        if let Some(value) = service.custom_settings.get("enable_dir_listing") {
            if let Some(enabled) = value.as_bool() {
                custom_settings.insert("enable_dir_listing".to_string(), enabled.to_string());
            }
        } else {
            custom_settings.insert("enable_dir_listing".to_string(), "false".to_string());
        }
        
        Ok(Self {
            service,
            global,
            custom_settings,
        })
    }

    /// Get the path to the Nginx configuration directory
    pub fn get_config_path(&self) -> PathBuf {
        self.service.local_path.clone()
    }

    /// Validate Nginx configuration
    pub async fn validate_config(&self) -> Result<bool> {
        info!("[{}] Validating Nginx configuration", self.service.name);
        
        // Try to use validation command if available
        if let Some(cmd) = &self.service.validation_command {
            info!("[{}] Running validation command: {}", self.service.name, cmd);
            
            let status = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .status()
                .await
                .context("Failed to execute validation command")?;
                
            if !status.success() {
                warn!("[{}] Validation command failed", self.service.name);
                return Ok(false);
            }
            
            info!("[{}] Validation command succeeded", self.service.name);
            return Ok(true);
        }
        
        // Fall back to standard nginx -t validation
        info!("[{}] No validation command specified, using standard nginx -t", self.service.name);
        
        let status = Command::new("docker")
            .args(&["exec", &self.service.container_name, "nginx", "-t"])
            .status()
            .await
            .context("Failed to execute nginx -t")?;
            
        if !status.success() {
            warn!("[{}] Nginx configuration test failed", self.service.name);
            return Ok(false);
        }
        
        info!("[{}] Nginx configuration test passed", self.service.name);
        return Ok(true);
    }
    
    /// Find all Nginx configuration files
    pub fn find_config_files(&self) -> Result<Vec<PathBuf>> {
        let dir = &self.service.local_path;
        let mut config_files = Vec::new();
        
        if !dir.exists() || !dir.is_dir() {
            return Ok(config_files);
        }
        
        for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
            let path = entry.path();
            
            if path.is_file() {
                let file_name = path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                
                if file_name == "nginx.conf" || file_name.ends_with(".conf") {
                    config_files.push(path.to_path_buf());
                }
            }
        }
        
        Ok(config_files)
    }
    
    /// Analyze and fix common Nginx configuration issues
    pub async fn fix_common_issues(&self) -> Result<()> {
        if !self.service.effective_auto_fix(self.global.auto_fix) {
            debug!("[{}] Auto-fix disabled, skipping issue fixing", self.service.name);
            return Ok(());
        }
        
        info!("[{}] Analyzing and fixing common Nginx configuration issues", self.service.name);
        
        let config_files = self.find_config_files()?;
        
        if config_files.is_empty() {
            warn!("[{}] No Nginx configuration files found in {}", 
                  self.service.name, self.service.local_path.display());
            return Ok(());
        }
        
        info!("[{}] Found {} Nginx configuration files", self.service.name, config_files.len());
        
        // Get directory listing setting
        let enable_dir_listing = self.custom_settings.get("enable_dir_listing")
            .map(|v| v == "true")
            .unwrap_or(false);
        
        // Get web root 
        let web_root = self.custom_settings.get("web_root")
            .unwrap_or(&"/var/www/html".to_string())
            .clone();
        
        // Check for common issues and fix them
        for config_file in &config_files {
            // Read the config file
            let content = fs::read_to_string(config_file)
                .context(format!("Failed to read config file: {}", config_file.display()))?;
            
            // Fix directory listing if requested
            if enable_dir_listing && content.contains("autoindex off;") {
                info!("[{}] Enabling directory listing in {}", self.service.name, config_file.display());
                
                let new_content = content.replace("autoindex off;", "autoindex on;");
                
                if new_content != content {
                    fs::write(config_file, new_content)
                        .context(format!("Failed to write changes to {}", config_file.display()))?;
                }
            }
            
            // Check for root directories and ensure they have index files
            let root_pattern = Regex::new(r"root\s+([^;]+)")?;
            
            for cap in root_pattern.captures_iter(&content) {
                if let Some(root_dir) = cap.get(1) {
                    let dir_path = root_dir.as_str().trim();
                    
                    // Skip if directory path contains variables
                    if dir_path.contains("${") || dir_path.contains("$") {
                        continue;
                    }
                    
                    let path = PathBuf::from(dir_path);
                    
                    // Create directory if it doesn't exist
                    if !path.exists() {
                        info!("[{}] Creating directory: {}", self.service.name, path.display());
                        fs::create_dir_all(&path)
                            .context(format!("Failed to create directory: {}", path.display()))?;
                    }
                    
                    if path.is_dir() {
                        let has_index = fs::read_dir(&path)
                            .context(format!("Failed to read directory: {}", path.display()))?
                            .filter_map(Result::ok)
                            .any(|entry| {
                                let name = entry.file_name().to_string_lossy();
                                name.starts_with("index.")
                            });
                        
                        if !has_index {
                            info!("[{}] Creating default index.html in {}", self.service.name, path.display());
                            
                            let index_path = path.join("index.html");
                            let mut file = File::create(&index_path)
                                .context(format!("Failed to create index file: {}", index_path.display()))?;
                            
                            file.write_all(b"<!DOCTYPE html>\n<html>\n<head>\n  <title>Welcome</title>\n</head>\n<body>\n  <h1>Welcome</h1>\n  <p>Site under construction</p>\n</body>\n</html>\n")
                                .context("Failed to write default content to index.html")?;
                        }
                    }
                }
            }
        }
        
        info!("[{}] Completed checking and fixing common issues", self.service.name);
        Ok(())
    }
    
    /// Fix permissions for Nginx files
    pub async fn fix_permissions(&self) -> Result<()> {
        if !self.service.effective_fix_permissions(self.global.fix_permissions) {
            debug!("[{}] Permission fixing is disabled", self.service.name);
            return Ok(());
        }
        
        if let Some(permissions) = &self.service.permissions {
            info!("[{}] Fixing permissions for Nginx files", self.service.name);
            self.fix_local_permissions(permissions).await?;
            self.fix_container_permissions(permissions).await?;
        } else {
            info!("[{}] No permission settings found, using defaults", self.service.name);
            // Use default permissions
            let default_permissions = Permissions {
                fix: true,
                user: "nginx".to_string(),
                group: "nginx".to_string(),
            };
            
            self.fix_local_permissions(&default_permissions).await?;
            self.fix_container_permissions(&default_permissions).await?;
        }
        
        Ok(())
    }
    
    /// Fix permissions in the local repository
    async fn fix_local_permissions(&self, permissions: &Permissions) -> Result<()> {
        let repo_path = &self.service.local_path;
        if !repo_path.exists() {
            return Ok(());
        }
        
        info!("[{}] Setting permissions for local repo at {}", 
              self.service.name, repo_path.display());
        
        let owner = format!("{}:{}", permissions.user, permissions.group);
        
        // Try to fix with user/group names
        let status = Command::new("chown")
            .args(&["-R", &owner, &repo_path.to_string_lossy()])
            .status()
            .await;
            
        if let Err(e) = status {
            warn!("[{}] Failed to set permissions with named users: {}", self.service.name, e);
            
            // Try with numeric IDs if available
            if let (Ok(uid), Ok(gid)) = (std::env::var("USER_ID"), std::env::var("GROUP_ID")) {
                info!("[{}] Trying with numeric IDs: {}:{}", self.service.name, uid, gid);
                
                let numeric_owner = format!("{}:{}", uid, gid);
                let status = Command::new("chown")
                    .args(&["-R", &numeric_owner, &repo_path.to_string_lossy()])
                    .status()
                    .await;
                    
                if let Err(e) = status {
                    warn!("[{}] Failed to set permissions with numeric IDs: {}", self.service.name, e);
                }
            }
        }
        
        // Set directory permissions
        let dir_chmod_status = Command::new("find")
            .args([
                &repo_path.to_string_lossy(),
                "-type", "d",
                "-exec", "chmod", "750", "{}", ";"
            ])
            .status()
            .await
            .context("Failed to set directory permissions")?;
        
        if !dir_chmod_status.success() {
            warn!("[{}] Failed to set directory permissions", self.service.name);
        }
        
        // Set file permissions
        let file_chmod_status = Command::new("find")
            .args([
                &repo_path.to_string_lossy(),
                "-type", "f",
                "-exec", "chmod", "640", "{}", ";"
            ])
            .status()
            .await
            .context("Failed to set file permissions")?;
        
        if !file_chmod_status.success() {
            warn!("[{}] Failed to set file permissions", self.service.name);
        }
        
        Ok(())
    }
    
    /// Fix permissions inside the container
    async fn fix_container_permissions(&self, permissions: &Permissions) -> Result<()> {
        // Check if container exists and is running
        let status = check_container_status(&self.service.container_name).await?;
        if status != ContainerStatus::Running {
            warn!("[{}] Container is not running, skipping container permission fixes", self.service.name);
            return Ok(());
        }
        
        // Get web root
        let web_root = self.custom_settings.get("web_root")
            .unwrap_or(&"/var/www/html".to_string())
            .clone();
            
        // Fix web root permissions inside container
        info!("[{}] Setting permissions for web root at {}", self.service.name, web_root);
        
        let cmd = format!(
            "mkdir -p {} && \
             chown -R {}:{} {} && \
             chmod -R 755 {} && \
             find {} -type d -exec chmod 755 {{}} \\; && \
             find {} -type f -exec chmod 644 {{}} \\;",
            web_root, permissions.user, permissions.group,
            web_root, web_root, web_root, web_root
        );
        
        let status = Command::new("docker")
            .args(["exec", "-u", "root", &self.service.container_name, "sh", "-c", &cmd])
            .status()
            .await
            .context("Failed to fix web root permissions")?;
        
        if !status.success() {
            warn!("[{}] Permission fixing command failed for web root", self.service.name);
        }
        
        // Create index files where missing
        info!("[{}] Creating default index files where missing", self.service.name);
        
        // Get list of all directories in web root
        let cmd = format!("find {} -type d", web_root);
        let output = Command::new("docker")
            .args(["exec", &self.service.container_name, "sh", "-c", &cmd])
            .output()
            .await
            .context("Failed to list directories in web root")?;
        
        if !output.status.success() {
            warn!("[{}] Failed to list directories in web root", self.service.name);
            return Ok(());
        }
        
        let dirs = String::from_utf8_lossy(&output.stdout);
        
        for dir in dirs.lines() {
            // Check if directory has index files
            let check_cmd = format!("find {} -maxdepth 1 -name \"index.*\" | grep .", dir);
            let check_result = Command::new("docker")
                .args(["exec", &self.service.container_name, "sh", "-c", &check_cmd])
                .output()
                .await;
            
            // If no index files found (grep returns non-zero), create one
            if check_result.is_err() || !check_result.unwrap().status.success() {
                info!("[{}] Creating default index.html in {}", self.service.name, dir);
                
                let create_cmd = format!(
                    "echo '<!DOCTYPE html>
<html>
<head>
  <title>Welcome</title>
</head>
<body>
  <h1>Welcome</h1>
  <p>Site under construction</p>
</body>
</html>' > {}/index.html && \
                    chown {}:{} {}/index.html && \
                    chmod 644 {}/index.html",
                    dir, permissions.user, permissions.group, dir, dir
                );
                
                let create_result = Command::new("docker")
                    .args(["exec", "-u", "root", &self.service.container_name, "sh", "-c", &create_cmd])
                    .status()
                    .await;
                
                if let Err(e) = create_result {
                    warn!("[{}] Failed to create index.html in {}: {}", self.service.name, dir, e);
                }
            }
        }
        
        // Fix Nginx configuration permissions
        info!("[{}] Setting correct permissions for Nginx configuration", self.service.name);
        
        let cmd = "chmod -R 644 /etc/nginx/conf.d/*.conf && chmod 644 /etc/nginx/nginx.conf";
        let status = Command::new("docker")
            .args(["exec", "-u", "root", &self.service.container_name, "sh", "-c", &cmd])
            .status()
            .await
            .context("Failed to fix Nginx configuration permissions")?;
        
        if !status.success() {
            warn!("[{}] Failed to fix Nginx configuration permissions", self.service.name);
        }
        
        Ok(())
    }
    
    /// Enhance Nginx security configuration
    pub async fn enhance_security(&self) -> Result<()> {
        info!("[{}] Enhancing Nginx security configuration", self.service.name);
        
        let config_files = self.find_config_files()?;
        
        if config_files.is_empty() {
            warn!("[{}] No Nginx configuration files found", self.service.name);
            return Ok(());
        }
        
        for config_file in &config_files {
            let content = fs::read_to_string(config_file)
                .context(format!("Failed to read config file: {}", config_file.display()))?;
            
            // Check and add security headers if not present
            if !content.contains("add_header X-Content-Type-Options") {
                info!("[{}] Adding security headers to {}", self.service.name, config_file.display());
                
                let mut lines: Vec<String> = content.lines().map(String::from).collect();
                
                // Find server blocks
                for i in 0..lines.len() {
                    if lines[i].contains("server {") || lines[i].trim() == "server {" {
                        // Look for a location block to add headers after
                        for j in i..lines.len() {
                            if lines[j].contains("location") && lines[j].contains("{") {
                                // Add security headers inside location block
                                if j + 1 < lines.len() {
                                    lines.insert(j + 1, "        # Security headers".to_string());
                                    lines.insert(j + 2, "        add_header X-Content-Type-Options nosniff;".to_string());
                                    lines.insert(j + 3, "        add_header X-Frame-Options SAMEORIGIN;".to_string());
                                    lines.insert(j + 4, "        add_header X-XSS-Protection \"1; mode=block\";".to_string());
                                    break;
                                }
                            }
                        }
                    }
                }
                
                // Write updated content
                let new_content = lines.join("\n");
                if new_content != content {
                    fs::write(config_file, new_content)
                        .context(format!("Failed to write changes to {}", config_file.display()))?;
                    
                    info!("[{}] Added security headers to {}", self.service.name, config_file.display());
                }
            }
        }
        
        info!("[{}] Security enhancement complete", self.service.name);
        Ok(())
    }
    
    /// Monitor Nginx logs
    pub async fn monitor_logs(&self) -> Result<Vec<String>> {
        if !self.service.effective_monitor_logs(self.global.monitor_logs) {
            debug!("[{}] Log monitoring is disabled", self.service.name);
            return Ok(vec![]);
        }
        
        info!("[{}] Monitoring Nginx logs", self.service.name);
        
        // Convert to a simplified NginxConfig and use the shared function
        let config = NginxConfig {
            nginx_container_name: self.service.container_name.clone(),
            compose_dir: PathBuf::new(), // Not needed for log checks
            compose_file: String::new(),  // Not needed for log checks
            use_docker_compose: false,    // Not needed for log checks
            disable_restart: false,       // Not needed for log checks
            monitor_logs: true,
            log_tail_lines: self.service.log_tail_lines,
            force_rebuild: None,
        };
        
        check_nginx_logs(&config).await?;
        
        // Additional detailed log analysis could be added here
        let container_running = check_container_status(&self.service.container_name).await?;
        if container_running != ContainerStatus::Running {
            return Ok(vec![]);
        }
        
        // Get error logs
        let output = Command::new("docker")
            .args(["exec", &self.service.container_name, "sh", "-c", 
                  &format!("tail -n {} /var/log/nginx/error.log", self.service.log_tail_lines)])
            .output()
            .await
            .context("Failed to get Nginx error logs")?;
        
        if !output.status.success() {
            warn!("[{}] Failed to retrieve Nginx error logs", self.service.name);
            return Ok(vec![]);
        }
        
        let logs = String::from_utf8_lossy(&output.stdout);
        let log_lines: Vec<String> = logs.lines()
            .filter(|line| {
                line.contains("error") || line.contains("critical") || 
                line.contains("alert") || line.contains("emerg")
            })
            .map(String::from)
            .collect();
        
        Ok(log_lines)
    }
}

/// Validates the Nginx configuration - standalone function for external use
pub async fn validate_nginx(service: &ServiceConfig, global: &GlobalSettings) -> Result<bool> {
    let nginx = NginxService::new(service, global)?;
    nginx.validate_config().await
}

/// Fix common Nginx issues - standalone function for external use 
pub async fn fix_issues(service: &ServiceConfig, global: &GlobalSettings) -> Result<()> {
    let nginx = NginxService::new(service, global)?;
    nginx.fix_common_issues().await?;
    nginx.enhance_security().await
}

/// Fix Nginx permissions - standalone function for external use
pub async fn fix_nginx_permissions(service: &ServiceConfig, global: &GlobalSettings) -> Result<()> {
    let nginx = NginxService::new(service, global)?;
    nginx.fix_permissions().await
}

/// ServiceHandler trait for polymorphic service operations
pub trait ServiceHandler {
    async fn validate(&self) -> Result<bool>;
    async fn fix_issues(&self) -> Result<()>;
    async fn fix_permissions(&self) -> Result<()>;
    async fn monitor(&self) -> Result<Vec<String>>;
}

/// Implement the ServiceHandler trait for NginxService
impl<'a> ServiceHandler for NginxService<'a> {
    async fn validate(&self) -> Result<bool> {
        self.validate_config().await
    }
    
    async fn fix_issues(&self) -> Result<()> {
        self.fix_common_issues().await?;
        self.enhance_security().await
    }
    
    async fn fix_permissions(&self) -> Result<()> {
        self.fix_permissions().await
    }
    
    async fn monitor(&self) -> Result<Vec<String>> {
        self.monitor_logs().await
    }
}

/// Create a service handler factory based on service type
pub fn create_service_handler<'a>(
    service: &'a ServiceConfig, 
    global: &'a GlobalSettings
) -> Result<Box<dyn ServiceHandler + 'a>> {
    match service.service_type {
        ServiceType::Nginx => {
            let nginx = NginxService::new(service, global)?;
            Ok(Box::new(nginx))
        },
        _ => Err(anyhow!("Service type not supported yet: {:?}", service.service_type)),
    }
}