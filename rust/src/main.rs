mod config;
mod docker;
mod git;
mod logger;
mod nginx;
mod utils;

use anyhow::{Context, Result};
use log::{error, info, warn};
use std::sync::Arc;
use tokio::signal;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};

use crate::config::{load_config, show_config};
use crate::docker::{check_container_status, check_nginx_logs, restart_nginx};
use crate::git::{pull_latest_changes, setup_repository};
use crate::nginx::{fix_issues, fix_permissions, validate_config};
use crate::utils::{check_dependencies, check_running_instance, remove_lock_file, setup_ssh_keys};

async fn run() -> Result<()> {
    // Load configuration
    let config = load_config()?;
    
    // Initialize logger
    logger::init(config.verbose);
    
    // Display configuration if verbose is enabled
    if config.verbose {
        show_config(&config);
    }
    
    // Check dependencies
    check_dependencies()?;
    
    // Check for running instance
    check_running_instance(&config.lockfile)?;
    
    // Handle cleanup on shutdown
    let lockfile = config.lockfile.clone();
    ctrlc::set_handler(move || {
        info!("Received shutdown signal, exiting gracefully");
        let _ = remove_lock_file(&lockfile);
        std::process::exit(0);
    })?;
    
    // Set up SSH authentication if provided
    if let Some(key) = &config.ssh_private_key {
        if let Err(e) = setup_ssh_keys(key).await {
            warn!("Failed to set up SSH keys: {}", e);
        }
    }
    
    // Set up Git repository
    let mut git_repo = setup_repository(&config)
        .context("Failed to set up Git repository")?;
    
    // Make config accessible in async contexts
    let config = Arc::new(config);
    
    // Notify health check URL if provided
    if let Some(url) = &config.healthcheck_url {
        if let Err(e) = logger::notify_healthcheck(url, "Monitoring started", false).await {
            warn!("Failed to ping health check URL: {}", e);
        }
    }
    
    // Initial permission fixing if enabled
    if config.fix_permissions {
        // Wait a moment for container to be ready
        sleep(Duration::from_secs(5)).await;
        
        if let Err(e) = fix_permissions(&config).await {
            warn!("Failed to fix permissions: {}", e);
        }
    }
    
    // Initial check of Nginx logs
    if let Err(e) = check_nginx_logs(&config).await {
        warn!("Failed to check Nginx logs: {}", e);
    }
    
    // Start the main monitoring loop
    info!("Starting monitoring of {} (branch: {})", config.repo_url, config.branch);
    info!("Checking every {} seconds", config.watch_interval);
    
    // Log configuration settings
    if config.auto_fix {
        info!("Auto-fix for common Nginx issues is ENABLED");
    } else if config.verbose {
        info!("Auto-fix for common Nginx issues is disabled");
    }
    
    if config.fix_permissions {
        info!("Nginx permission fixing is ENABLED");
    } else if config.verbose {
        info!("Nginx permission fixing is disabled");
    }
    
    if config.enable_dir_listing {
        warn!("Directory listing (autoindex) is ENABLED - this may expose sensitive files");
    } else if config.verbose {
        info!("Directory listing is disabled (secure default)");
    }
    
    if config.monitor_logs {
        info!("Nginx log monitoring is ENABLED");
    } else if config.verbose {
        info!("Nginx log monitoring is disabled");
    }
    
    // Wrap git_repo in Mutex for safe concurrent access
    let git_repo = Arc::new(Mutex::new(git_repo));
    
    // Main loop
    loop {
        let config_ref = Arc::clone(&config);
        let git_repo_ref = Arc::clone(&git_repo);
        
        // Pull latest changes
        let result = async {
            let mut git_repo = git_repo_ref.lock().await;
            let changes_detected = pull_latest_changes(&mut git_repo, &config_ref).await?;
            
            if changes_detected {
                // Validate Nginx configuration
                validate_config(&config_ref).await?;
                
                // Fix issues if needed
                if config_ref.auto_fix {
                    fix_issues(&config_ref).await?;
                }
                
                // Restart Nginx
                restart_nginx(&config_ref).await?;
                
                // Fix permissions if enabled
                if config_ref.fix_permissions {
                    sleep(Duration::from_secs(2)).await;
                    fix_permissions(&config_ref).await?;
                }
            } else {
                // Check for Nginx errors even if there are no Git changes
                check_nginx_logs(&config_ref).await?;
            }
            
            Ok::<_, anyhow::Error>(())
        }.await;
        
        if let Err(e) = result {
            error!("Update cycle failed: {}", e);
            warn!("Will retry next interval");
            
            // Notify health check URL if provided
            if let Some(url) = &config.healthcheck_url {
                let _ = logger::notify_healthcheck(url, &format!("Error: {}", e), true).await;
            }
        }
        
        // Wait for next check
        if config.verbose {
            info!("Sleeping for {} seconds", config.watch_interval);
        }
        
        sleep(Duration::from_secs(config.watch_interval)).await;
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}