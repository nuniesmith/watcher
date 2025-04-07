use anyhow::{anyhow, Context, Result};
use log::{debug, error, info, warn};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::sync::Arc;
use tokio::signal::ctrl_c;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{sleep, Duration};

use crate::config::{Config, GlobalSettings, ServiceConfig, ServiceType};
use crate::docker_utils::ContainerStatus;
use crate::git::service as git_service;
use crate::nginx::{check_nginx_logs, restart_nginx};
use crate::service::{check_service_status, restart_service, run_validation};
use crate::utils::fix_permissions;

/// Main entry point for the application
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging from environment
    env_logger::init_from_env(
        env_logger::Env::default().filter_or("RUST_LOG", "info")
    );

    // Load configuration
    let config = match Config::load() {
        Ok(cfg) => {
            cfg.display();
            cfg
        },
        Err(e) => {
            error!("Failed to load configuration: {}", e);
            return Err(e);
        }
    };

    // Wrap in Arc for sharing between tasks
    let config = Arc::new(config);
    
    // Write PID to lockfile
    let pid = process::id();
    let lockfile = PathBuf::from("/var/run/config_watcher.lock");
    
    info!("Starting config watcher (PID: {})", pid);
    info!("Writing lockfile: {}", lockfile.display());
    
    if let Err(e) = File::create(&lockfile).and_then(|mut file| {
        file.write_all(pid.to_string().as_bytes())
    }) {
        warn!("Failed to write lockfile: {}", e);
    }

    // Setup signal handler channel
    let (tx, mut rx) = mpsc::channel(1);
    let tx_clone = tx.clone();

    // Handle Ctrl+C signal
    tokio::spawn(async move {
        if let Err(e) = ctrl_c().await {
            error!("Failed to listen for Ctrl+C: {}", e);
            return;
        }
        info!("Received shutdown signal. Stopping...");
        let _ = tx_clone.send(()).await;
    });

    // Set up task set for monitoring services
    let mut tasks = JoinSet::new();
    
    // Create a task for each service
    for (idx, service) in config.services.iter().enumerate() {
        let service_config = service.clone();
        let global_config = config.global_settings.clone();
        let tx = tx.clone();
        
        info!("Starting monitoring task for service: {}", service.name);
        
        tasks.spawn(async move {
            monitor_service(service_config, global_config, idx, tx).await
        });
    }

    // Wait for shutdown signal or task completion
    tokio::select! {
        _ = rx.recv() => {
            info!("Shutdown signal received, stopping all tasks...");
            tasks.abort_all();
        }
        res = tasks.join_next() => {
            if let Some(result) = res {
                match result {
                    Ok(service_result) => {
                        match service_result {
                            Ok(name) => info!("Service task for '{}' completed", name),
                            Err(e) => error!("Service task failed with error: {}", e),
                        }
                    }
                    Err(e) => error!("Task join error: {}", e),
                }
            }
            // If one task ended, trigger shutdown for all
            let _ = tx.send(()).await;
        }
    }

    // Cleanup lockfile
    if lockfile.exists() {
        if let Err(e) = std::fs::remove_file(&lockfile) {
            warn!("Failed to remove lockfile: {}", e);
        }
    }

    info!("Config Watcher shutdown complete");
    Ok(())
}

/// Monitor a single service for changes
async fn monitor_service(
    service: ServiceConfig, 
    global: GlobalSettings,
    idx: usize,
    shutdown_tx: mpsc::Sender<()>
) -> Result<String> {
    let service_name = service.name.clone();
    info!("Starting monitoring for service: {}", service_name);
    
    // Startup grace period
    let grace_period = parse_duration(&global.startup_grace_period)
        .unwrap_or_else(|_| Duration::from_secs(30));
    
    info!("[{}] Waiting {} seconds for startup grace period", 
          service_name, grace_period.as_secs());
    sleep(grace_period).await;
    
    // Ensure the repository is properly initialized
    match git_service::init_repository(&service, &global).await {
        Ok(_) => info!("[{}] Git repository initialized", service_name),
        Err(e) => {
            error!("[{}] Failed to initialize repository: {}", service_name, e);
            return Err(e.into());
        }
    }
    
    // Set watch interval
    let watch_interval = Duration::from_secs(global.watch_interval);
    
    // Main monitoring loop
    loop {
        info!("[{}] Checking for updates...", service_name);
        
        // Check for updates in the repository
        match git_service::check_for_updates(&service, &global).await {
            Ok(updated) => {
                if updated {
                    info!("[{}] Updates detected, applying changes", service_name);
                    
                    // Handle service-specific updates
                    match service.service_type {
                        ServiceType::Nginx => {
                            handle_nginx_update(&service, &global, idx).await?;
                        },
                        ServiceType::Apache => {
                            handle_apache_update(&service, &global).await?;
                        },
                        ServiceType::Generic | ServiceType::Custom(_) => {
                            handle_generic_update(&service, &global).await?;
                        }
                    }
                } else {
                    info!("[{}] No updates detected", service_name);
                    
                    // Periodic checks even if no updates
                    if service.service_type == ServiceType::Nginx && 
                       service.effective_monitor_logs(global.monitor_logs) {
                        // Create a simplified nginx config for the specific service
                        if let Ok(nginx_config) = Config::make_nginx_config(&service, &global) {
                            if let Err(e) = check_nginx_logs(&nginx_config).await {
                                warn!("[{}] Error checking Nginx logs: {}", service_name, e);
                            }
                        }
                    }
                }
            },
            Err(e) => {
                error!("[{}] Error checking for updates: {}", service_name, e);
            }
        }
        
        // Wait for next check interval
        debug!("[{}] Sleeping for {} seconds", service_name, watch_interval.as_secs());
        sleep(watch_interval).await;
    }
}

/// Handle Nginx-specific service updates
async fn handle_nginx_update(service: &ServiceConfig, global: &GlobalSettings, idx: usize) -> Result<()> {
    let service_name = &service.name;
    
    // Create a simplified nginx config for this specific service
    let nginx_config = Config::make_nginx_config(service, global)
        .context(format!("Failed to create Nginx config for service {}", service_name))?;
    
    // Run validation command if specified
    if let Some(cmd) = &service.validation_command {
        info!("[{}] Running validation command", service_name);
        if let Err(e) = run_validation(service, cmd).await {
            error!("[{}] Validation failed: {}", service_name, e);
            
            // If auto-fix is enabled, attempt to fix by reverting changes
            if service.effective_auto_fix(global.auto_fix) {
                info!("[{}] Auto-fix enabled, attempting to revert changes", service_name);
                if let Err(e) = git_service::revert_changes(service, global).await {
                    error!("[{}] Failed to revert changes: {}", service_name, e);
                }
            }
            
            return Err(anyhow!("Validation failed for service {}", service_name));
        }
    }
    
    // Apply permission fixes if configured
    if service.effective_fix_permissions(global.fix_permissions) {
        if let Some(perms) = &service.permissions {
            info!("[{}] Fixing permissions to {}:{}", service_name, perms.user, perms.group);
            if let Err(e) = fix_permissions(service, perms).await {
                warn!("[{}] Failed to fix permissions: {}", service_name, e);
            }
        }
    }
    
    // Restart service if not disabled
    if !service.disable_restart && !global.disable_restart {
        info!("[{}] Restarting Nginx service", service_name);
        if let Err(e) = restart_nginx(&nginx_config).await {
            error!("[{}] Failed to restart Nginx: {}", service_name, e);
            return Err(e.into());
        }
    }
    
    // Check logs if monitoring is enabled
    if service.effective_monitor_logs(global.monitor_logs) {
        if let Err(e) = check_nginx_logs(&nginx_config).await {
            warn!("[{}] Error checking Nginx logs: {}", service_name, e);
        }
    }
    
    Ok(())
}

/// Handle Apache-specific service updates
async fn handle_apache_update(service: &ServiceConfig, global: &GlobalSettings) -> Result<()> {
    let service_name = &service.name;
    
    // Run validation if specified
    if let Some(cmd) = &service.validation_command {
        info!("[{}] Running validation command", service_name);
        if let Err(e) = run_validation(service, cmd).await {
            error!("[{}] Validation failed: {}", service_name, e);
            
            // If auto-fix is enabled, revert changes
            if service.effective_auto_fix(global.auto_fix) {
                info!("[{}] Auto-fix enabled, attempting to revert changes", service_name);
                if let Err(e) = git_service::revert_changes(service, global).await {
                    error!("[{}] Failed to revert changes: {}", service_name, e);
                }
            }
            
            return Err(anyhow!("Validation failed for service {}", service_name));
        }
    }
    
    // Apply permission fixes
    if service.effective_fix_permissions(global.fix_permissions) {
        if let Some(perms) = &service.permissions {
            info!("[{}] Fixing permissions to {}:{}", service_name, perms.user, perms.group);
            if let Err(e) = fix_permissions(service, perms).await {
                warn!("[{}] Failed to fix permissions: {}", service_name, e);
            }
        }
    }
    
    // Restart service
    if !service.disable_restart && !global.disable_restart {
        info!("[{}] Restarting Apache service", service_name);
        if let Err(e) = restart_service(service, global).await {
            error!("[{}] Failed to restart Apache: {}", service_name, e);
            return Err(e.into());
        }
    }
    
    Ok(())
}

/// Handle generic service updates
async fn handle_generic_update(service: &ServiceConfig, global: &GlobalSettings) -> Result<()> {
    let service_name = &service.name;
    
    // Run validation if specified
    if let Some(cmd) = &service.validation_command {
        info!("[{}] Running validation command", service_name);
        if let Err(e) = run_validation(service, cmd).await {
            error!("[{}] Validation failed: {}", service_name, e);
            
            // If auto-fix is enabled, revert changes
            if service.effective_auto_fix(global.auto_fix) {
                info!("[{}] Auto-fix enabled, attempting to revert changes", service_name);
                if let Err(e) = git_service::revert_changes(service, global).await {
                    error!("[{}] Failed to revert changes: {}", service_name, e);
                }
            }
            
            return Err(anyhow!("Validation failed for service {}", service_name));
        }
    }
    
    // Apply permission fixes
    if service.effective_fix_permissions(global.fix_permissions) {
        if let Some(perms) = &service.permissions {
            info!("[{}] Fixing permissions to {}:{}", service_name, perms.user, perms.group);
            if let Err(e) = fix_permissions(service, perms).await {
                warn!("[{}] Failed to fix permissions: {}", service_name, e);
            }
        }
    }
    
    // Restart service
    if !service.disable_restart && !global.disable_restart {
        info!("[{}] Restarting service", service_name);
        if let Err(e) = restart_service(service, global).await {
            error!("[{}] Failed to restart service: {}", service_name, e);
            return Err(e.into());
        }
    }
    
    Ok(())
}

/// Parse a duration string (e.g., "30s", "5m") into a Duration
fn parse_duration(duration_str: &str) -> Result<Duration> {
    let len = duration_str.len();
    if len < 2 {
        return Err(anyhow!("Invalid duration format: {}", duration_str));
    }
    
    let (value_str, unit) = duration_str.split_at(len - 1);
    let value = value_str.parse::<u64>()
        .context(format!("Failed to parse duration value: {}", value_str))?;
    
    match unit {
        "s" => Ok(Duration::from_secs(value)),
        "m" => Ok(Duration::from_secs(value * 60)),
        "h" => Ok(Duration::from_secs(value * 3600)),
        "d" => Ok(Duration::from_secs(value * 86400)),
        _ => Err(anyhow!("Invalid duration unit: {}", unit)),
    }
}