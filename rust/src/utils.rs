use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use tokio::time::{sleep, Duration};

pub fn check_running_instance(lockfile: &Path) -> Result<()> {
    if lockfile.exists() {
        // Read the PID from the lockfile
        let pid = fs::read_to_string(lockfile)
            .context("Failed to read PID from lockfile")?
            .trim()
            .parse::<u32>()
            .context("Failed to parse PID from lockfile")?;
        
        // Check if process is still running
        #[cfg(unix)]
        {
            let proc_path = format!("/proc/{}", pid);
            if Path::new(&proc_path).exists() {
                return Err(anyhow!("Script is already running with PID {}", pid));
            }
        }
        
        #[cfg(windows)]
        {
            let output = Command::new("tasklist")
                .args(&["/FI", &format!("PID eq {}", pid), "/NH"])
                .output()
                .context("Failed to execute tasklist command")?;
            
            if String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()) {
                return Err(anyhow!("Script is already running with PID {}", pid));
            }
        }
        
        // If we reach here, the process is not running
        warn!("Found stale lockfile, removing");
        fs::remove_file(lockfile)
            .context("Failed to remove stale lockfile")?;
    }
    
    // Create lockfile with current PID
    let pid = std::process::id();
    let mut file = File::create(lockfile)
        .context("Failed to create lockfile")?;
    
    writeln!(file, "{}", pid)
        .context("Failed to write PID to lockfile")?;
    
    info!("Created lockfile with PID {}", pid);
    Ok(())
}

pub fn remove_lock_file(lockfile: &Path) -> Result<()> {
    if lockfile.exists() {
        fs::remove_file(lockfile)
            .context("Failed to remove lockfile")?;
    }
    Ok(())
}

pub fn check_dependencies() -> Result<()> {
    // Check for git
    if Command::new("git").arg("--version").output().is_err() {
        return Err(anyhow!("Git is not installed or not in PATH"));
    }
    
    // Check for Docker
    if Command::new("docker").arg("--version").output().is_err() {
        return Err(anyhow!("Docker is not installed or not in PATH"));
    }
    
    // Check for Docker Compose
    let docker_compose_v2 = Command::new("docker")
        .args(["compose", "version"])
        .output();
    
    let docker_compose_legacy = Command::new("docker-compose")
        .arg("--version")
        .output();
    
    if docker_compose_v2.is_err() && docker_compose_legacy.is_err() {
        warn!("Neither 'docker compose' nor 'docker-compose' are available in PATH");
    }
    
    Ok(())
}

pub async fn setup_ssh_keys(ssh_key: &str) -> Result<PathBuf> {
    if ssh_key.is_empty() {
        return Err(anyhow!("Empty SSH key provided"));
    }
    
    info!("Setting up SSH keys");
    
    // Create ~/.ssh directory if it doesn't exist
    let ssh_dir = dirs::home_dir()
        .ok_or_else(|| anyhow!("Could not determine home directory"))?
        .join(".ssh");
    
    fs::create_dir_all(&ssh_dir)
        .context("Failed to create .ssh directory")?;
    
    // Write the SSH key to a file
    let key_path = ssh_dir.join("id_rsa_nginx_watcher");
    let mut file = File::create(&key_path)
        .context("Failed to create SSH key file")?;
    
    file.write_all(ssh_key.as_bytes())
        .context("Failed to write SSH key")?;
    
    // Set correct permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&key_path, permissions)
            .context("Failed to set SSH key file permissions")?;
    }
    
    // Add known hosts
    let known_hosts_path = ssh_dir.join("known_hosts");
    
    for host in &["github.com", "gitlab.com", "bitbucket.org", "azure.com"] {
        // Skip if host is already in known_hosts
        if let Ok(content) = fs::read_to_string(&known_hosts_path) {
            if content.contains(host) {
                continue;
            }
        }
        
        // Use ssh-keyscan to add the host
        let output = Command::new("ssh-keyscan")
            .arg(host)
            .output()
            .context(format!("Failed to scan host key for {}", host))?;
        
        if !output.status.success() {
            warn!("Failed to add {} to known hosts", host);
            continue;
        }
        
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&known_hosts_path)
            .context("Failed to open known_hosts file")?;
        
        file.write_all(&output.stdout)
            .context(format!("Failed to write {} key to known_hosts", host))?;
    }
    
    Ok(key_path)
}