use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ContainerStatus {
    Running,
    Stopped,
    NotExists,
}

/// Check the current status of a Docker container
pub async fn check_container_status(container_name: &str) -> Result<ContainerStatus> {
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
    
    debug!("Container {} does not exist", container_name);
    Ok(ContainerStatus::NotExists)
}

/// Restart a Docker container or start it if stopped
pub async fn restart_container(container_name: &str) -> Result<()> {
    let status = check_container_status(container_name).await?;
    
    match status {
        ContainerStatus::Running => {
            info!("Restarting running container {}", container_name);
            execute_docker_command(&["restart", container_name], "restart").await?;
        },
        ContainerStatus::Stopped => {
            info!("Starting stopped container {}", container_name);
            execute_docker_command(&["start", container_name], "start").await?;
        },
        ContainerStatus::NotExists => {
            return Err(anyhow!("Container {} does not exist and cannot be restarted", container_name));
        }
    }
    
    // Wait for container to fully start
    sleep(Duration::from_secs(2)).await;
    
    Ok(())
}

/// Get logs from a Docker container
pub async fn get_container_logs(container_name: &str, tail_lines: u32) -> Result<String> {
    let output = Command::new("docker")
        .args(["logs", "--tail", &tail_lines.to_string(), container_name])
        .output()
        .await
        .context(format!("Failed to get logs for container {}", container_name))?;
    
    let logs = String::from_utf8(output.stdout)
        .context("Failed to parse container logs as UTF-8")?;
    
    let stderr = String::from_utf8(output.stderr)
        .context("Failed to parse container stderr logs as UTF-8")?;
    
    // Combine stdout and stderr logs
    Ok(format!("{}\n{}", logs, stderr))
}

/// Execute a Docker command and handle errors
async fn execute_docker_command(args: &[&str], operation: &str) -> Result<()> {
    let status = Command::new("docker")
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context(format!("Failed to execute docker {} command", operation))?;
    
    if !status.success() {
        return Err(anyhow!("Docker {} command failed with exit code: {:?}", 
                           operation, status.code()));
    }
    
    Ok(())
}

/// Configuration for Docker Compose operations
pub struct DockerComposeConfig {
    pub compose_dir: PathBuf,
    pub compose_file: Option<String>,
    pub service_name: String,
}

/// Detect which Docker Compose command to use (V2 or legacy)
pub async fn detect_docker_compose_command() -> (String, bool) {
    let docker_compose_v2 = Command::new("docker")
        .args(["compose", "version"])
        .output()
        .await;
    
    if docker_compose_v2.is_ok() {
        ("docker compose".to_string(), true)
    } else {
        ("docker-compose".to_string(), false)
    }
}

/// Restart a service using Docker Compose
pub async fn restart_with_docker_compose(config: &DockerComposeConfig) -> Result<()> {
    let (compose_cmd, _is_v2) = detect_docker_compose_command().await;
    
    // Check if compose file exists
    let compose_file = get_compose_file_arg(config)?;
    
    // Execute docker-compose restart
    info!("Restarting container {} with Docker Compose", config.service_name);
    
    let restart_cmd = format!("cd {} && {} {} restart {}", 
                            config.compose_dir.display(), 
                            compose_cmd, 
                            compose_file,
                            config.service_name);
    
    let status = Command::new("sh")
        .arg("-c")
        .arg(&restart_cmd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("Failed to execute docker-compose restart command")?;
    
    if !status.success() {
        return Err(anyhow!("Docker Compose restart command failed with exit code: {:?}", status.code()));
    }
    
    info!("Container {} restarted successfully with Docker Compose", config.service_name);
    
    // Wait for container to be fully up
    sleep(Duration::from_secs(5)).await;
    
    Ok(())
}

/// Recreate containers using Docker Compose (down, build, up)
pub async fn recreate_with_docker_compose(config: &DockerComposeConfig) -> Result<()> {
    let (compose_cmd, _is_v2) = detect_docker_compose_command().await;
    
    // Check if compose file exists
    let compose_file = get_compose_file_arg(config)?;
    
    // Execute docker-compose down
    info!("Stopping containers with Docker Compose");
    let down_cmd = format!("cd {} && {} {} down", 
                         config.compose_dir.display(), 
                         compose_cmd, 
                         compose_file);
    
    let down_status = Command::new("sh")
        .arg("-c")
        .arg(&down_cmd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("Failed to execute docker-compose down command")?;
    
    if !down_status.success() {
        warn!("Docker Compose down command failed, continuing anyway");
    }
    
    // Execute docker-compose build
    info!("Building containers with Docker Compose");
    let build_cmd = format!("cd {} && {} {} build", 
                          config.compose_dir.display(), 
                          compose_cmd, 
                          compose_file);
    
    let build_status = Command::new("sh")
        .arg("-c")
        .arg(&build_cmd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("Failed to execute docker-compose build command")?;
    
    if !build_status.success() {
        return Err(anyhow!("Docker Compose build command failed"));
    }
    
    // Execute docker-compose up
    info!("Starting containers with Docker Compose");
    let up_cmd = format!("cd {} && {} {} up -d", 
                       config.compose_dir.display(), 
                       compose_cmd, 
                       compose_file);
    
    let up_status = Command::new("sh")
        .arg("-c")
        .arg(&up_cmd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("Failed to execute docker-compose up command")?;
    
    if !up_status.success() {
        return Err(anyhow!("Docker Compose up command failed"));
    }
    
    info!("Containers recreated successfully with Docker Compose");
    
    // Wait for containers to be fully up
    sleep(Duration::from_secs(5)).await;
    
    Ok(())
}

/// Get the compose file argument, checking for file existence
fn get_compose_file_arg(config: &DockerComposeConfig) -> Result<String> {
    if let Some(file) = &config.compose_file {
        let file_path = config.compose_dir.join(file);
        if file_path.exists() {
            return Ok(format!("-f {}", file));
        }
    }
    
    // Check for default files
    let compose_file_path = config.compose_dir.join("docker-compose.yml");
    let compose_yml_path = config.compose_dir.join("compose.yml");
    
    if compose_file_path.exists() {
        Ok("-f docker-compose.yml".to_string())
    } else if compose_yml_path.exists() {
        Ok("-f compose.yml".to_string())
    } else {
        Err(anyhow!("No docker-compose.yml or compose.yml file found in {}", 
                   config.compose_dir.display()))
    }
}