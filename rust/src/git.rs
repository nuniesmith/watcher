use anyhow::{Context, Result};
use git2::{build::RepoBuilder, Cred, FetchOptions, RemoteCallbacks, Repository};
use log::{debug, error, info, warn};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;
use crate::config::Config;

pub struct GitRepo {
    pub repo: Repository,
    pub current_commit: String,
}

pub fn setup_repository(config: &Config) -> Result<GitRepo> {
    let path = &config.config_dir;
    
    // Check if repository already exists
    if path.join(".git").exists() {
        info!("Git repository already exists at {}", path.display());
        
        // Open existing repository
        let repo = Repository::open(path)
            .context("Failed to open existing repository")?;
        
        // Check current branch
        let head = repo.head()?;
        let current_branch = head.shorthand().unwrap_or("unknown").to_string();
        
        if current_branch != config.branch {
            warn!("Switching from branch {} to {}", current_branch, config.branch);
            
            // Stash any uncommitted changes
            let statuses = repo.statuses(None)?;
            if !statuses.is_empty() {
                warn!("Found uncommitted changes, stashing them");
                let signature = repo.signature()?;
                repo.stash_save(&signature, "Auto-stash before branch switch", None)?;
            }
            
            // Try to find the branch locally first
            let mut found_locally = false;
            let branches = repo.branches(None)?;
            for branch_result in branches {
                let (branch, _) = branch_result?;
                if branch.name()?.map(|s| s == config.branch).unwrap_or(false) {
                    found_locally = true;
                    break;
                }
            }
            
            if found_locally {
                // Local branch exists, check it out
                let obj = repo.revparse_single(&config.branch)?;
                repo.checkout_tree(&obj, None)?;
                
                let branch_ref = format!("refs/heads/{}", config.branch);
                repo.set_head(&branch_ref)?;
            } else {
                // Need to fetch the branch
                let mut remote = repo.find_remote("origin")?;
                
                let mut callbacks = RemoteCallbacks::new();
                if let Some(key) = &config.ssh_private_key {
                    setup_auth_callbacks(&mut callbacks, key);
                }
                
                let mut fetch_options = FetchOptions::new();
                fetch_options.remote_callbacks(callbacks);
                
                remote.fetch(&[&config.branch], Some(&mut fetch_options), None)?;
                
                // Try to find the remote branch
                let remote_branch_ref = format!("refs/remotes/origin/{}", config.branch);
                if let Ok(remote_branch) = repo.find_reference(&remote_branch_ref) {
                    let oid = remote_branch.target().unwrap();
                    let commit = repo.find_commit(oid)?;
                    
                    // Create a local branch that tracks the remote branch
                    repo.branch(&config.branch, &commit, false)?;
                    
                    // Checkout the branch
                    let obj = repo.revparse_single(&config.branch)?;
                    repo.checkout_tree(&obj, None)?;
                    
                    let branch_ref = format!("refs/heads/{}", config.branch);
                    repo.set_head(&branch_ref)?;
                } else {
                    return Err(anyhow::anyhow!("Branch {} not found on remote", config.branch));
                }
            }
        }
        
        // Get current commit hash
        let head = repo.head()?;
        let head_commit = head.peel_to_commit()?;
        let current_commit = head_commit.id().to_string();
        
        info!("Current commit: {}", current_commit);
        
        Ok(GitRepo {
            repo,
            current_commit,
        })
    } else {
        // Repository doesn't exist, clone it
        info!("Cloning repository {} to {}", config.repo_url, path.display());
        
        // Create directory if it doesn't exist
        if path.exists() {
            warn!("Directory exists but is not a git repository. Removing contents.");
            fs::remove_dir_all(path)?;
        }
        
        fs::create_dir_all(path)?;
        
        // Setup authentication if private key is provided
        let mut callbacks = RemoteCallbacks::new();
        if let Some(key) = &config.ssh_private_key {
            setup_auth_callbacks(&mut callbacks, key);
        }
        
        let mut fetch_options = FetchOptions::new();
        fetch_options.remote_callbacks(callbacks);
        
        let mut builder = RepoBuilder::new();
        builder.fetch_options(fetch_options);
        builder.branch(&config.branch);
        
        let repo = builder.clone(&config.repo_url, path)
            .context("Failed to clone repository")?;
        
        // Get current commit hash
        let head = repo.head()?;
        let head_commit = head.peel_to_commit()?;
        let current_commit = head_commit.id().to_string();
        
        info!("Repository cloned successfully. Current commit: {}", current_commit);
        
        Ok(GitRepo {
            repo,
            current_commit,
        })
    }
}

pub async fn pull_latest_changes(git_repo: &mut GitRepo, config: &Config) -> Result<bool> {
    debug!("Pulling latest changes");
    
    let repo = &git_repo.repo;
    let repo_path = config.config_dir.to_str().unwrap();
    
    // Save current commit for potential rollback
    let previous_commit = git_repo.current_commit.clone();
    
    // Fetch latest changes
    info!("Fetching latest changes from remote");
    
    // Using standard git command for consistency with shell script
    let fetch_result = Command::new("git")
        .current_dir(repo_path)
        .args(["fetch", "origin", &config.branch])
        .output()
        .context("Failed to fetch from remote")?;
    
    if !fetch_result.status.success() {
        let error = String::from_utf8_lossy(&fetch_result.stderr);
        return Err(anyhow::anyhow!("Failed to fetch: {}", error));
    }
    
    // Get the latest commit hash
    let remote_ref = format!("refs/remotes/origin/{}", config.branch);
    let remote_branch = repo.find_reference(&remote_ref)?;
    let remote_commit = remote_branch.target()
        .ok_or_else(|| anyhow::anyhow!("Invalid reference target"))?
        .to_string();
    
    debug!("Remote commit: {}", remote_commit);
    debug!("Current commit: {}", git_repo.current_commit);
    
    // Check if there are changes
    if remote_commit != git_repo.current_commit {
        info!("Changes detected, pulling latest code");
        
        // Check for local changes
        let statuses = repo.statuses(None)?;
        let has_local_changes = !statuses.is_empty();
        
        if has_local_changes {
            warn!("Local uncommitted changes detected. Stashing them.");
            
            // Using git stash for simplicity
            let stash_result = Command::new("git")
                .current_dir(repo_path)
                .args(["stash"])
                .output()
                .context("Failed to stash changes")?;
            
            if !stash_result.status.success() {
                let error = String::from_utf8_lossy(&stash_result.stderr);
                warn!("Failed to stash changes: {}", error);
            }
        }
        
        // Pull changes
        let pull_result = Command::new("git")
            .current_dir(repo_path)
            .args(["pull", "origin", &config.branch])
            .output()
            .context("Failed to pull changes")?;
        
        if !pull_result.status.success() {
            let error = String::from_utf8_lossy(&pull_result.stderr);
            error!("Failed to pull changes: {}", error);
            
            // Check for merge conflicts
            if error.contains("CONFLICT") || error.contains("Automatic merge failed") {
                error!("Merge conflicts detected. Reverting to previous state.");
                
                // Reset to previous commit
                let reset_result = Command::new("git")
                    .current_dir(repo_path)
                    .args(["reset", "--hard", &previous_commit])
                    .output()
                    .context("Failed to reset to previous commit")?;
                
                if !reset_result.status.success() {
                    let reset_error = String::from_utf8_lossy(&reset_result.stderr);
                    error!("Failed to reset to previous commit: {}", reset_error);
                }
                
                return Err(anyhow::anyhow!("Merge conflicts detected"));
            }
            
            return Err(anyhow::anyhow!("Failed to pull changes: {}", error));
        }
        
        // Update current commit
        git_repo.current_commit = remote_commit;
        
        // Apply stashed changes if any
        if has_local_changes {
            info!("Applying stashed changes");
            
            let pop_result = Command::new("git")
                .current_dir(repo_path)
                .args(["stash", "pop"])
                .output();
            
            match pop_result {
                Ok(output) => {
                    if !output.status.success() {
                        let error = String::from_utf8_lossy(&output.stderr);
                        warn!("Failed to apply stashed changes: {}", error);
                    }
                },
                Err(e) => {
                    warn!("Error applying stashed changes: {}", e);
                }
            }
        }
        
        Ok(true) // Changes detected
    } else {
        debug!("No changes detected");
        Ok(false) // No changes
    }
}

fn setup_auth_callbacks(callbacks: &mut RemoteCallbacks, ssh_key: &str) {
    callbacks.credentials(move |_url, username_from_url, _allowed_types| {
        let username = username_from_url.unwrap_or("git");
        
        // Create a temporary file for the SSH key
        let mut temp_file = tempfile::Builder::new()
            .prefix("git_ssh_key")
            .suffix("")
            .tempfile()
            .expect("Failed to create temporary file for SSH key");
        
        std::io::Write::write_all(&mut temp_file, ssh_key.as_bytes())
            .expect("Failed to write SSH key to temporary file");
        
        let temp_path = temp_file.into_temp_path();
        
        // Set correct permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600))
                .expect("Failed to set permissions on SSH key file");
        }
        
        // Create SSH key credentials
        Cred::ssh_key(
            username,
            None, // No public key needed
            &temp_path,
            None, // No passphrase
        )
    });
}