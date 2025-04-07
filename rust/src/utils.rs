use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::time::Duration;
use url::Url;

use crate::config::{Permissions, ServiceConfig};

//--------------------------------
// Process Management Functions
//--------------------------------

/// Check for a running instance using a lockfile
pub async fn check_running_instance(lockfile: &Path) -> Result<()> {
    if lockfile.exists() {
        // Read the PID from the lockfile
        let pid = fs::read_to_string(lockfile)
            .context("Failed to read PID from lockfile")?
            .trim()
            .parse::<u32>()
            .context("Failed to parse PID from lockfile")?;
        
        // Check if process is still running
        if is_process_running(pid).await? {
            return Err(anyhow!("Process is already running with PID {}", pid));
        }
        
        // If we reach here, the process is not running
        warn!("Found stale lockfile at {}, removing", lockfile.display());
        fs::remove_file(lockfile)
            .context("Failed to remove stale lockfile")?;
    }
    
    // Create lockfile with current PID
    create_pid_file(lockfile).context("Failed to create lockfile")?;
    
    Ok(())
}

/// Remove a lock file
pub async fn remove_lock_file(lockfile: &Path) -> Result<()> {
    if lockfile.exists() {
        tokio::fs::remove_file(lockfile).await
            .context(format!("Failed to remove lockfile: {}", lockfile.display()))?;
        
        info!("Removed lockfile: {}", lockfile.display());
    }
    
    Ok(())
}

/// Create a PID file
pub fn create_pid_file(path: impl AsRef<Path>) -> Result<()> {
    let pid = std::process::id();
    let mut file = File::create(path.as_ref())
        .context(format!("Failed to create PID file: {}", path.as_ref().display()))?;
    
    writeln!(file, "{}", pid)
        .context("Failed to write PID to file")?;
    
    info!("Created PID file: {} (PID: {})", path.as_ref().display(), pid);
    Ok(())
}

/// Check if a PID file exists and is valid, returning the PID if active
pub async fn check_pid_file(path: impl AsRef<Path>) -> Result<Option<u32>> {
    let path = path.as_ref();
    
    if !path.exists() {
        return Ok(None);
    }
    
    let pid_str = fs::read_to_string(path)
        .context(format!("Failed to read PID file: {}", path.display()))?;
    
    let pid = pid_str.trim().parse::<u32>()
        .context(format!("Failed to parse PID from file: {}", path.display()))?;
    
    // Check if the process is still running
    if is_process_running(pid).await? {
        Ok(Some(pid))
    } else {
        // Stale PID file
        tokio::fs::remove_file(path).await
            .context(format!("Failed to remove stale PID file: {}", path.display()))?;
        
        Ok(None)
    }
}

/// Check if a process with the given PID is running
pub async fn is_process_running(pid: u32) -> Result<bool> {
    #[cfg(unix)]
    {
        let proc_path = format!("/proc/{}", pid);
        return Ok(Path::new(&proc_path).exists());
    }
    
    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(&["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .await
            .context(format!("Failed to execute tasklist command for PID {}", pid))?;
        
        Ok(String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
    }
    
    // Default implementation for other platforms
    #[cfg(not(any(unix, windows)))]
    {
        let output = Command::new("ps")
            .args(&["-p", &pid.to_string()])
            .output()
            .await
            .context(format!("Failed to execute ps command for PID {}", pid))?;
        
        Ok(output.status.success())
    }
}

//--------------------------------
// Dependency Checking
//--------------------------------

/// Check if required system dependencies are available
pub async fn check_dependencies() -> Result<()> {
    info!("Checking for required dependencies");
    
    // Essential dependencies
    let dependencies = ["git", "docker", "chown", "chmod", "find"];
    
    for dep in dependencies {
        let status = Command::new("which")
            .arg(dep)
            .status()
            .await
            .context(format!("Failed to check for dependency: {}", dep))?;
        
        if !status.success() {
            return Err(anyhow!("Required dependency not found: {}", dep));
        }
    }
    
    // Check for Docker Compose
    let docker_compose_v2 = Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await;
    
    let docker_compose_legacy = Command::new("docker-compose")
        .arg("--version")
        .output()
        .await;
    
    if docker_compose_v2.is_err() && docker_compose_legacy.is_err() {
        warn!("Neither 'docker compose' nor 'docker-compose' are available in PATH");
    } else {
        debug!("Docker Compose is available");
    }
    
    info!("All required dependencies are available");
    Ok(())
}

//--------------------------------
// Permission Management
//--------------------------------

/// Fix permissions for a service's files
pub async fn fix_permissions(service: &ServiceConfig, permissions: &Permissions) -> Result<()> {
    let path = &service.local_path;
    let user = &permissions.user;
    let group = &permissions.group;
    
    debug!("[{}] Fixing permissions for {} to {}:{}", 
           service.name, path.display(), user, group);
    
    // Make sure directory exists first
    if !path.exists() {
        return Err(anyhow!("Directory does not exist: {}", path.display()));
    }
    
    // Fix ownership
    let chown_status = Command::new("chown")
        .args(["-R", &format!("{}:{}", user, group), &path.to_string_lossy()])
        .status()
        .await
        .context(format!("Failed to execute chown command for {}", service.name))?;
    
    if !chown_status.success() {
        // Try with numeric IDs if available
        if let (Ok(uid), Ok(gid)) = (std::env::var("USER_ID"), std::env::var("GROUP_ID")) {
            warn!("[{}] Failed to fix permissions with named user/group, trying with numeric IDs: {}:{}", 
                  service.name, uid, gid);
            
            let numeric_owner = format!("{}:{}", uid, gid);
            let status = Command::new("chown")
                .args(["-R", &numeric_owner, &path.to_string_lossy()])
                .status()
                .await
                .context(format!("Failed to execute chown command with numeric IDs for {}", service.name))?;
            
            if !status.success() {
                return Err(anyhow!("Failed to change ownership for {}, even with numeric IDs", service.name));
            }
        } else {
            return Err(anyhow!("Failed to change ownership to {}:{} for {}", user, group, service.name));
        }
    }
    
    // Fix directory permissions
    let dir_chmod_status = Command::new("find")
        .args([
            &path.to_string_lossy(),
            "-type", "d",
            "-exec", "chmod", "750", "{}", ";"
        ])
        .status()
        .await
        .context(format!("Failed to execute chmod for directories in {}", service.name))?;
    
    if !dir_chmod_status.success() {
        warn!("[{}] Failed to set directory permissions to 750", service.name);
    }
    
    // Fix file permissions
    let file_chmod_status = Command::new("find")
        .args([
            &path.to_string_lossy(),
            "-type", "f",
            "-exec", "chmod", "640", "{}", ";"
        ])
        .status()
        .await
        .context(format!("Failed to execute chmod for files in {}", service.name))?;
    
    if !file_chmod_status.success() {
        warn!("[{}] Failed to set file permissions to 640", service.name);
    }
    
    // Fix execution permissions for scripts
    let script_chmod_status = Command::new("find")
        .args([
            &path.to_string_lossy(),
            "-type", "f",
            "-name", "*.sh",
            "-exec", "chmod", "750", "{}", ";"
        ])
        .status()
        .await
        .context(format!("Failed to execute chmod for script files in {}", service.name))?;
    
    if !script_chmod_status.success() {
        warn!("[{}] Failed to set script permissions to 750", service.name);
    }
    
    info!("[{}] Fixed permissions for {} to {}:{}", 
          service.name, path.display(), user, group);
    Ok(())
}

//--------------------------------
// SSH Key Management
//--------------------------------

/// Setup SSH authentication for Git
pub async fn setup_ssh_auth(key_content: &str) -> Result<PathBuf> {
    if key_content.trim().is_empty() {
        return Err(anyhow!("Empty SSH key provided"));
    }
    
    info!("Setting up SSH keys for Git authentication");
    
    // Create ~/.ssh directory if it doesn't exist
    let ssh_dir = dirs::home_dir()
        .ok_or_else(|| anyhow!("Could not determine home directory"))?
        .join(".ssh");
    
    if !ssh_dir.exists() {
        tokio::fs::create_dir_all(&ssh_dir).await
            .context("Failed to create .ssh directory")?;
        
        // Set proper permissions on .ssh directory
        let chmod_status = Command::new("chmod")
            .args(["700", &ssh_dir.to_string_lossy()])
            .status()
            .await
            .context("Failed to set permissions on .ssh directory")?;
        
        if !chmod_status.success() {
            return Err(anyhow!("Failed to set permissions on .ssh directory"));
        }
    }
    
    // Write the key to a file
    let key_path = ssh_dir.join("id_rsa_config_watcher");
    tokio::fs::write(&key_path, key_content).await
        .context("Failed to write SSH key file")?;
    
    // Set correct permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&key_path, permissions)
            .context("Failed to set SSH key file permissions")?;
    }
    
    #[cfg(not(unix))]
    {
        let chmod_status = Command::new("chmod")
            .args(["600", &key_path.to_string_lossy()])
            .status()
            .await
            .context("Failed to set permissions on SSH key file")?;
        
        if !chmod_status.success() {
            return Err(anyhow!("Failed to set permissions on SSH key file"));
        }
    }
    
    // Add to known hosts for common Git providers
    let known_hosts_path = ssh_dir.join("known_hosts");
    
    for host in &["github.com", "gitlab.com", "bitbucket.org", "azure.com"] {
        // Skip if host is already in known_hosts
        if let Ok(content) = fs::read_to_string(&known_hosts_path) {
            if content.contains(host) {
                continue;
            }
        }
        
        info!("Adding {} to known hosts", host);
        
        // Use ssh-keyscan to add the host
        let output = Command::new("ssh-keyscan")
            .arg(host)
            .output()
            .await
            .context(format!("Failed to scan host key for {}", host))?;
        
        if !output.status.success() {
            warn!("Failed to add {} to known hosts", host);
            continue;
        }
        
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&known_hosts_path)
            .context("Failed to open known_hosts file")?;
        
        file.write_all(&output.stdout)
            .context(format!("Failed to write {} key to known_hosts", host))?;
    }
    
    // Add key to ssh-agent if it's running
    if Path::new("/var/run/ssh-agent.sock").exists() || std::env::var("SSH_AUTH_SOCK").is_ok() {
        info!("Adding key to ssh-agent");
        
        let _ = Command::new("ssh-add")
            .arg(&key_path)
            .status()
            .await;
    }
    
    info!("SSH authentication setup complete");
    Ok(key_path)
}

//--------------------------------
// Health Check Notifications
//--------------------------------

/// Notify a health check service
pub async fn notify_healthcheck(url: &str, message: &str, is_error: bool) -> Result<()> {
    // Validate URL
    let parsed_url = Url::parse(url)
        .context(format!("Invalid health check URL: {}", url))?;
    
    debug!("Notifying health check service: {}", parsed_url);
    
    // Build the full URL with message
    let full_url = if is_error {
        format!("{}?status=fail&msg={}", url, urlencoding::encode(message))
    } else {
        format!("{}?msg={}", url, urlencoding::encode(message))
    };
    
    // Send the request
    let client = reqwest::Client::new();
    let response = client.get(&full_url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .context("Failed to send health check notification")?;
    
    if !response.status().is_success() {
        return Err(anyhow!(
            "Health check notification failed with status: {}", 
            response.status()
        ));
    }
    
    info!("Health check notification sent successfully");
    Ok(())
}

//--------------------------------
// Duration and Time Functions
//--------------------------------

/// Parse a duration string (e.g., "30s", "5m") into a Duration
pub fn parse_duration(duration_str: &str) -> Result<Duration> {
    let duration_str = duration_str.trim();
    
    // Extract numeric value
    let numeric: String = duration_str.chars()
        .take_while(|c| c.is_digit(10))
        .collect();
    
    if numeric.is_empty() {
        return Err(anyhow!("Invalid duration format: {}", duration_str));
    }
    
    let value = numeric.parse::<u64>()
        .context(format!("Failed to parse duration value: {}", numeric))?;
    
    // Extract unit (s, m, h, etc.)
    let unit: String = duration_str.chars()
        .skip_while(|c| c.is_digit(10))
        .collect::<String>()
        .to_lowercase();
    
    match unit.as_str() {
        "s" => Ok(Duration::from_secs(value)),
        "m" => Ok(Duration::from_secs(value * 60)),
        "h" => Ok(Duration::from_secs(value * 3600)),
        "d" => Ok(Duration::from_secs(value * 86400)),
        "" => Ok(Duration::from_secs(value)), // Assume seconds if no unit
        _ => Err(anyhow!("Invalid duration unit: {}", unit)),
    }
}

//--------------------------------
// File and Directory Functions
//--------------------------------

/// Create a directory if it doesn't exist
pub async fn ensure_directory(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    
    if !path.exists() {
        tokio::fs::create_dir_all(path).await
            .context(format!("Failed to create directory: {}", path.display()))?;
        
        debug!("Created directory: {}", path.display());
    }
    
    Ok(())
}

/// Check if a file exists and is readable
pub async fn check_file_accessible(path: impl AsRef<Path>) -> Result<bool> {
    let path = path.as_ref();
    
    if !path.exists() {
        return Ok(false);
    }
    
    let metadata = tokio::fs::metadata(path).await
        .context(format!("Failed to access file: {}", path.display()))?;
    
    Ok(metadata.is_file())
}

//--------------------------------
// Testing Functions
//--------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    
    #[tokio::test]
    async fn test_parse_duration() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
        assert_eq!(parse_duration("60").unwrap(), Duration::from_secs(60));
        
        assert!(parse_duration("invalid").is_err());
        assert!(parse_duration("30x").is_err());
    }
    
    #[tokio::test]
    async fn test_pid_file() -> Result<()> {
        let temp_dir = tempdir()?;
        let pid_file = temp_dir.path().join("test.pid");
        
        create_pid_file(&pid_file)?;
        assert!(pid_file.exists());
        
        let pid = check_pid_file(&pid_file).await?;
        assert!(pid.is_some());
        assert_eq!(pid.unwrap(), std::process::id());
        
        remove_lock_file(&pid_file).await?;
        assert!(!pid_file.exists());
        
        Ok(())
    }
}