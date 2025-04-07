use anyhow::{Context, Result};
use log::{debug, info, warn};
use regex::Regex;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use walkdir::WalkDir;

use crate::config::Config;
use crate::docker::check_container_status;
use crate::docker::ContainerStatus;

pub async fn validate_config(config: &Config) -> Result<bool> {
    info!("Validating Nginx configuration");
    
    // Find Nginx configuration files
    let nginx_configs = find_nginx_configs(&config.config_dir)?;
    
    if nginx_configs.is_empty() {
        warn!("No Nginx configuration files found in {}", config.config_dir.display());
        return Ok(false);
    }
    
    info!("Found {} Nginx configuration files", nginx_configs.len());
    
    // Check for common configuration issues
    let mut issues_found = false;
    
    // Check for 'deny all' directives which might cause 403 errors
    for config_file in &nginx_configs {
        let content = fs::read_to_string(config_file)
            .context(format!("Failed to read config file: {}", config_file.display()))?;
        
        if content.contains("deny all") {
            warn!("Found 'deny all' directive in {} that might cause 403 errors", config_file.display());
            issues_found = true;
        }
        
        // Check for missing index files in root directories
        let root_pattern = Regex::new(r"root\s+([^;]+)")?;
        
        for cap in root_pattern.captures_iter(&content) {
            if let Some(root_dir) = cap.get(1) {
                let dir_path = root_dir.as_str().trim();
                
                // Skip if directory path contains variables
                if dir_path.contains("${") || dir_path.contains("$") {
                    continue;
                }
                
                let path = PathBuf::from(dir_path);
                if path.exists() && path.is_dir() {
                    let has_index = fs::read_dir(&path)
                        .context(format!("Failed to read directory: {}", path.display()))?
                        .filter_map(Result::ok)
                        .any(|entry| {
                            let name = entry.file_name().to_string_lossy();
                            name.starts_with("index.")
                        });
                    
                    if !has_index {
                        warn!("Directory {} exists but has no index.* files, which may cause 403 errors", path.display());
                        issues_found = true;
                    }
                }
            }
        }
    }
    
    Ok(!issues_found)
}

pub fn find_nginx_configs(dir: &Path) -> Result<Vec<PathBuf>> {
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

pub async fn fix_issues(config: &Config) -> Result<()> {
    if !config.auto_fix {
        return Ok(());
    }
    
    info!("Attempting to fix common Nginx configuration issues");
    
    let nginx_configs = find_nginx_configs(&config.config_dir)?;
    
    // Create default index.html files where missing
    for config_file in &nginx_configs {
        let content = fs::read_to_string(config_file)
            .context(format!("Failed to read config file: {}", config_file.display()))?;
        
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
                    info!("Creating directory: {}", path.display());
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
                        info!("Creating default index.html in {}", path.display());
                        
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
    
    // Enable directory listing if requested
    if config.enable_dir_listing {
        warn!("Enabling directory listing (autoindex) as requested");
        
        for config_file in &nginx_configs {
            let content = fs::read_to_string(config_file)
                .context(format!("Failed to read config file: {}", config_file.display()))?;
            
            let new_content = content.replace("autoindex off;", "autoindex on;");
            
            if new_content != content {
                fs::write(config_file, new_content)
                    .context(format!("Failed to write changes to {}", config_file.display()))?;
                
                info!("Updated autoindex setting in {}", config_file.display());
            }
        }
    }
    
    Ok(())
}

pub async fn fix_permissions(config: &Config) -> Result<()> {
    if !config.fix_permissions {
        debug!("Permission fixing is disabled");
        return Ok(());
    }
    
    info!("Fixing permissions in Nginx container");
    
    // Check if container is running
    if check_container_status(config).await? != ContainerStatus::Running {
        warn!("Cannot fix permissions - Nginx container is not running");
        return Ok(());
    }
    
    // Fix web root permissions
    info!("Setting correct ownership and permissions for web content");
    
    let cmd = format!(
        "mkdir -p {}/honeybun && \
         chown -R {}:{} {} && \
         chmod -R 755 {} && \
         find {} -type d -exec chmod 755 {{}} \\; && \
         find {} -type f -exec chmod 644 {{}} \\;",
        config.web_root, config.nginx_user, config.nginx_group,
        config.web_root, config.web_root, config.web_root, config.web_root
    );
    
    let status = Command::new("docker")
        .args(["exec", "-u", "root", &config.nginx_container_name, "sh", "-c", &cmd])
        .status()
        .await
        .context("Failed to fix web root permissions")?;
    
    if !status.success() {
        warn!("Permission fixing command failed for web root");
    }
    
    // Find directories without index files and create default ones
    info!("Creating default index files where missing");
    
    // Get list of all directories in web root
    let cmd = format!("find {} -type d", config.web_root);
    let output = Command::new("docker")
        .args(["exec", &config.nginx_container_name, "sh", "-c", &cmd])
        .output()
        .await
        .context("Failed to list directories in web root")?;
    
    let dirs = String::from_utf8_lossy(&output.stdout);
    
    for dir in dirs.lines() {
        // Check if directory has index files
        let check_cmd = format!("find {} -maxdepth 1 -name \"index.*\" | grep .", dir);
        let check_result = Command::new("docker")
            .args(["exec", &config.nginx_container_name, "sh", "-c", &check_cmd])
            .output()
            .await;
        
        // If no index files found (grep returns non-zero), create one
        if check_result.is_err() || !check_result.unwrap().status.success() {
            info!("Creating default index.html in {}", dir);
            
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
                dir, config.nginx_user, config.nginx_group, dir, dir
            );
            
            let create_result = Command::new("docker")
                .args(["exec", "-u", "root", &config.nginx_container_name, "sh", "-c", &create_cmd])
                .status()
                .await;
            
            if let Err(e) = create_result {
                warn!("Failed to create index.html in {}: {}", dir, e);
            }
        }
    }
    
    // Fix Nginx configuration permissions
    info!("Setting correct permissions for Nginx configuration");
    
    let cmd = "chmod -R 644 /etc/nginx/conf.d/*.conf && chmod 644 /etc/nginx/nginx.conf";
    let status = Command::new("docker")
        .args(["exec", "-u", "root", &config.nginx_container_name, "sh", "-c", &cmd])
        .status()
        .await
        .context("Failed to fix Nginx configuration permissions")?;
    
    if !status.success() {
        warn!("Failed to fix Nginx configuration permissions");
    }
    
    // Test Nginx configuration
    info!("Testing Nginx configuration");
    
    let status = Command::new("docker")
        .args(["exec", &config.nginx_container_name, "nginx", "-t"])
        .status()
        .await
        .context("Failed to test Nginx configuration")?;
    
    if !status.success() {
        warn!("Nginx configuration test failed");
    } else {
        info!("Nginx configuration test passed");
    }
    
    info!("Permission fixing complete");
    Ok(())
}