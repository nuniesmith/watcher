use anyhow::{anyhow, Context, Result};
use log::{debug, error, info, warn};
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use crate::config::{GlobalSettings, ServiceConfig, ServiceType};
use crate::docker_utils::{
    ContainerStatus, DockerComposeConfig, check_container_status, restart_container,
    restart_with_docker_compose, recreate_with_docker_compose
};

/// Default command timeout in seconds
const DEFAULT_COMMAND_TIMEOUT: u64 = 60;

/// Run validation command for a service
pub async fn run_validation(service: &ServiceConfig, validation_cmd: &str) -> Result<()> {
    info!("[{}] Running validation command: {}", service.name, validation_cmd);
    
    let result = timeout(
        Duration::from_secs(DEFAULT_COMMAND_TIMEOUT),
        Command::new("sh")
            .arg("-c")
            .arg(validation_cmd)
            .output()
    ).await
        .context("Validation command timed out")?
        .context(format!("Failed to execute validation command for service {}", service.name))?;
    
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let stdout = String::from_utf8_lossy(&result.stdout);
        
        error!("[{}] Validation failed with exit code: {:?}", service.name, result.status.code());
        if !stderr.is_empty() {
            error!("[{}] Validation error output: {}", service.name, stderr);
        }
        if !stdout.is_empty() {
            debug!("[{}] Validation output: {}", service.name, stdout);
        }
        
        return Err(anyhow!("Validation command failed for service {} with exit code: {:?}",
                           service.name, result.status.code()));
    }
    
    info!("[{}] Validation successful", service.name);
    Ok(())
}

/// Restart a service based on its configuration
pub async fn restart_service(service: &ServiceConfig, global: &GlobalSettings) -> Result<()> {
    // Skip if restart is disabled
    if service.disable_restart || global.disable_restart {
        info!("[{}] Service restart is disabled by configuration. Skipping.", service.name);
        return Ok(());
    }
    
    info!("[{}] Restarting service", service.name);
    
    // Use the appropriate restart method based on service type and configuration
    match service.service_type {
        ServiceType::Nginx | ServiceType::Apache => {
            restart_web_service(service, global).await
        },
        _ => {
            restart_generic_service(service, global).await
        }
    }
}

/// Restart a web service (Nginx/Apache)
async fn restart_web_service(service: &ServiceConfig, global: &GlobalSettings) -> Result<()> {
    // Check if we should use a custom restart command
    if let Some(cmd) = &service.restart_command {
        info!("[{}] Using custom restart command: {}", service.name, cmd);
        return execute_custom_command(cmd, service).await;
    }
    
    // Check if service exists and is running
    let status = check_service_status(service).await?;
    
    // Use Docker Compose if configured, otherwise use plain Docker
    if service.use_docker_compose || global.use_docker_compose {
        restart_with_compose(service, global, status).await
    } else {
        match status {
            ContainerStatus::Running => {
                info!("[{}] Restarting running container", service.name);
                restart_container(&service.container_name).await
            },
            ContainerStatus::Stopped => {
                info!("[{}] Starting stopped container", service.name);
                restart_container(&service.container_name).await
            },
            ContainerStatus::NotExists => {
                error!("[{}] Container does not exist", service.name);
                Err(anyhow!("Container {} does not exist and cannot be restarted", 
                           service.container_name))
            }
        }
    }
}

/// Restart a generic service
async fn restart_generic_service(service: &ServiceConfig, global: &GlobalSettings) -> Result<()> {
    // Check if we should use a custom restart command
    if let Some(cmd) = &service.restart_command {
        info!("[{}] Using custom restart command: {}", service.name, cmd);
        return execute_custom_command(cmd, service).await;
    }
    
    // Otherwise use Docker or Docker Compose based on config
    if service.use_docker_compose || global.use_docker_compose {
        let status = check_service_status(service).await?;
        restart_with_compose(service, global, status).await
    } else {
        restart_container(&service.container_name).await
    }
}

/// Execute a custom shell command
async fn execute_custom_command(cmd: &str, service: &ServiceConfig) -> Result<()> {
    let result = timeout(
        Duration::from_secs(DEFAULT_COMMAND_TIMEOUT),
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
    ).await
        .context("Custom command timed out")?
        .context(format!("Failed to execute custom command for service {}", service.name))?;
    
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let stdout = String::from_utf8_lossy(&result.stdout);
        
        error!("[{}] Custom command failed with exit code: {:?}", service.name, result.status.code());
        if !stderr.is_empty() {
            error!("[{}] Command error output: {}", service.name, stderr);
        }
        if !stdout.is_empty() {
            debug!("[{}] Command output: {}", service.name, stdout);
        }
        
        return Err(anyhow!("Custom command failed for service {} with exit code: {:?}",
                           service.name, result.status.code()));
    }
    
    info!("[{}] Custom command executed successfully", service.name);
    Ok(())
}

/// Restart service using Docker Compose
async fn restart_with_compose(
    service: &ServiceConfig, 
    global: &GlobalSettings,
    status: ContainerStatus
) -> Result<()> {
    // Determine compose directory
    let compose_dir = resolve_compose_directory(service, global)?;
    
    // Get compose file
    let compose_file = service.docker_compose_file.clone()
        .or_else(|| global.default_compose_file.clone());
    
    // Build the compose config
    let compose_config = DockerComposeConfig {
        compose_dir,
        compose_file,
        service_name: service.container_name.clone(),
    };
    
    match status {
        ContainerStatus::NotExists => {
            info!("[{}] Container does not exist, recreating with docker-compose", service.name);
            recreate_with_docker_compose(&compose_config).await
        },
        _ => {
            info!("[{}] Restarting with docker-compose", service.name);
            restart_with_docker_compose(&compose_config).await
        }
    }
}

/// Resolve the Docker Compose directory, checking service config then global config
fn resolve_compose_directory(service: &ServiceConfig, global: &GlobalSettings) -> Result<PathBuf> {
    // Check service-specific directory first
    if let Some(dir) = &service.docker_compose_dir {
        return Ok(dir.clone());
    }
    
    // Check global default directory
    if let Some(dir) = &global.default_compose_dir {
        return Ok(dir.clone());
    }
    
    // Use service path if available
    if service.local_path.exists() && service.local_path.is_dir() {
        return Ok(service.local_path.clone());
    }
    
    // Fall back to current directory
    std::env::current_dir()
        .context("Failed to determine current directory and no compose directory was specified")
}

/// Check if a service container exists and is running
pub async fn check_service_status(service: &ServiceConfig) -> Result<ContainerStatus> {
    debug!("[{}] Checking container status", service.name);
    check_container_status(&service.container_name).await
}

/// Wait for a service to become ready (container running)
pub async fn wait_for_service_ready(
    service: &ServiceConfig, 
    max_attempts: u32, 
    delay: Duration
) -> Result<bool> {
    info!("[{}] Waiting for service to become ready", service.name);
    
    for attempt in 1..=max_attempts {
        debug!("[{}] Checking service readiness (attempt {}/{})", 
               service.name, attempt, max_attempts);
        
        let status = check_service_status(service).await?;
        
        if status == ContainerStatus::Running {
            info!("[{}] Service is ready and running", service.name);
            return Ok(true);
        }
        
        if attempt < max_attempts {
            tokio::time::sleep(delay).await;
        }
    }
    
    warn!("[{}] Service not ready after {} attempts", service.name, max_attempts);
    Ok(false)
}

/// ServiceHandler trait for working with custom service types
pub trait ServiceHandler {
    /// Get the service name
    fn name(&self) -> &str;
    
    /// Get the service type
    fn service_type(&self) -> ServiceType;
    
    /// Validate the service configuration
    async fn validate(&self) -> Result<bool>;
    
    /// Restart the service
    async fn restart(&self) -> Result<()>;
    
    /// Fix service-specific issues
    async fn fix_issues(&self) -> Result<()>;
    
    /// Fix service permissions
    async fn fix_permissions(&self) -> Result<()>;
    
    /// Monitor service logs and status
    async fn monitor(&self) -> Result<()>;
}