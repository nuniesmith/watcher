use anyhow::{anyhow, Context, Result};
use log::{debug, error, info, warn};
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::sleep;
use tokio::time::Duration;
use crate::config::Config;

#[derive(Debug, PartialEq)]
pub enum ContainerStatus {
    Running,
    Stopped,
    NotExists,
}

pub async fn check_container_status(config: &Config) -> Result<ContainerStatus> {
    let container_name = &config.nginx_container_name;
    
    // Check running containers
    let output = Command::new("docker")
        .args(["ps", "--format", "{{.Names}}", "--filter", &format!("name=^{}$", container_name)])
        .output()
        .await
        .context("Failed to execute docker ps command")?;
    
    let containers = String::from_utf8_lossy(&output.stdout).trim().to_string();
    
    if containers.contains(container_name) {
        debug!("Container {} is running", container_name);
        return Ok(ContainerStatus::Running);
    }
    
    // Check all containers (including stopped ones)
    let output = Command::new("docker")
        .args(["ps", "-a", "--format", "{{.Names}}", "--filter", &format!("name=^{}$", container_name)])
        .output()
        .await
        .context("Failed to execute docker ps -a command")?;
    
    let containers = String::from_utf8_lossy(&output.stdout).trim().to_string();
    
    if containers.contains(container_name) {
        warn!("Container {} exists but is not running", container_name);
        return Ok(ContainerStatus::Stopped);
    }
    
    warn!("Container {} does not exist", container_name);
    Ok(ContainerStatus::NotExists)
}

pub async fn restart_nginx(config: &Config) -> Result<()> {
    if config.disable_restart {
        info!("Container restart is disabled by configuration. Skipping.");
        return Ok(());
    }
    
    info!("Restarting Nginx container: {}", config.nginx_container_name);
    
    if config.use_docker_compose {
        restart_with_compose(config).await
    } else {
        restart_with_docker(config).await
    }
}

async fn restart_with_compose(config: &Config) -> Result<()> {
    let compose_dir = &config.compose_dir;
    let compose_file = &config.compose_file;
    
    // Check if compose file exists
    let compose_file_path = compose_dir.join(compose_file);
    let compose_yml_path = compose_dir.join("compose.yml");
    
    if !compose_file_path.exists() && !compose_yml_path.exists() {
        return Err(anyhow!("No {} or compose.yml file found in {}", 
                          compose_file, compose_dir.display()));
    }
    
    // Check which docker compose command to use
    let docker_compose_v2 = Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await;
    
    let (compose_cmd, is_v2) = if docker_compose_v2.is_ok() {
        ("docker compose", true)
    } else {
        ("docker-compose", false)
    };
    
    info!("Using {} command", compose_cmd);
    
    // Prepare file argument if needed
    let file_arg = if compose_file_path.exists() {
        format!("-f {}", compose_file)
    } else {
        String::new()
    };
    
    // Execute docker-compose down
    info!("Stopping containers with docker-compose");
    let down_cmd = if file_arg.is_empty() {
        format!("cd {} && {} down", compose_dir.display(), compose_cmd)
    } else {
        format!("cd {} && {} {} down", compose_dir.display(), compose_cmd, file_arg)
    };
    
    let down_status = Command::new("sh")
        .arg("-c")
        .arg(&down_cmd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("Failed to execute docker-compose down command")?;
    
    if !down_status.success() {
        warn!("docker-compose down command failed, continuing anyway");
    }
    
    // Execute docker-compose build
    info!("Building containers with docker-compose");
    let build_cmd = if file_arg.is_empty() {
        format!("cd {} && {} build", compose_dir.display(), compose_cmd)
    } else {
        format!("cd {} && {} {} build", compose_dir.display(), compose_cmd, file_arg)
    };
    
    let build_status = Command::new("sh")
        .arg("-c")
        .arg(&build_cmd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("Failed to execute docker-compose build command")?;
    
    if !build_status.success() {
        return Err(anyhow!("docker-compose build command failed"));
    }
    
    // Execute docker-compose up
    info!("Starting containers with docker-compose");
    let up_cmd = if file_arg.is_empty() {
        format!("cd {} && {} up -d", compose_dir.display(), compose_cmd)
    } else {
        format!("cd {} && {} {} up -d", compose_dir.display(), compose_cmd, file_arg)
    };
    
    let up_status = Command::new("sh")
        .arg("-c")
        .arg(&up_cmd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("Failed to execute docker-compose up command")?;
    
    if !up_status.success() {
        return Err(anyhow!("docker-compose up command failed"));
    }
    
    info!("Nginx container restarted successfully with Docker Compose");
    
    // Wait for container to be fully up before fixing permissions
    sleep(Duration::from_secs(5)).await;
    
    Ok(())
}

async fn restart_with_docker(config: &Config) -> Result<()> {
    let container_status = check_container_status(config).await?;
    
    match container_status {
        ContainerStatus::Running => {
            // Container is running, just restart it
            info!("Restarting running container {}", config.nginx_container_name);
            
            let status = Command::new("docker")
                .args(["restart", &config.nginx_container_name])
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await
                .context("Failed to execute docker restart command")?;
            
            if !status.success() {
                return Err(anyhow!("Failed to restart Nginx container"));
            }
            
            info!("Nginx container restarted successfully");
        },
        ContainerStatus::Stopped => {
            // Container exists but is not running
            info!("Starting stopped container {}", config.nginx_container_name);
            
            let status = Command::new("docker")
                .args(["start", &config.nginx_container_name])
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await
                .context("Failed to execute docker start command")?;
            
            if !status.success() {
                return Err(anyhow!("Failed to start Nginx container"));
            }
            
            info!("Nginx container started successfully");
        },
        ContainerStatus::NotExists => {
            return Err(anyhow!("Nginx container does not exist and cannot be restarted without Docker Compose"));
        }
    }
    
    // Wait for container to be fully up before fixing permissions
    sleep(Duration::from_secs(2)).await;
    
    Ok(())
}

pub async fn check_nginx_logs(config: &Config) -> Result<()> {
    if !config.monitor_logs {
        debug!("Log monitoring is disabled");
        return Ok(());
    }
    
    info!("Checking Nginx logs for errors");
    
    // Check if container is running
    let status = check_container_status(config).await?;
    if status != ContainerStatus::Running {
        warn!("Cannot check logs - Nginx container is not running");
        return Ok(());
    }
    
    // Get logs from the container
    let output = Command::new("docker")
        .args(["logs", "--tail", &config.log_tail_lines.to_string(), &config.nginx_container_name])
        .output()
        .await
        .context("Failed to get Nginx logs")?;
    
    let logs = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    
    // Check for errors
    let errors: Vec<&str> = logs.lines()
        .chain(stderr.lines())
        .filter(|line| line.to_lowercase().contains("error"))
        .collect();
    
    if !errors.is_empty() {
        warn!("Found errors in Nginx logs:");
        
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