use anyhow::{anyhow, Context, Result};
use log::{debug, error, info, warn};
use std::fs;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tempfile::NamedTempFile;
use crate::config::{ServiceConfig, GlobalSettings};

/// Git repository manager for handling repository operations
pub struct GitRepo {
    /// Path to the local repository
    pub path: PathBuf,
    /// URL of the remote repository
    pub remote_url: String,
    /// Branch to work with
    pub branch: String,
    /// Current commit hash
    pub current_commit: Option<String>,
    /// SSH private key for authentication (if provided)
    ssh_key: Option<String>,
}

impl GitRepo {
    /// Create a new GitRepo instance
    pub fn new(path: PathBuf, url: String, branch: String, ssh_key: Option<String>) -> Self {
        Self {
            path,
            remote_url: url,
            branch,
            current_commit: None,
            ssh_key,
        }
    }

    /// Create from service configuration
    pub fn from_service(service: &ServiceConfig, global: &GlobalSettings) -> Self {
        let branch = service.effective_branch(&global.default_branch);
        
        Self {
            path: service.local_path.clone(),
            remote_url: service.repo_url.clone(),
            branch,
            current_commit: None,
            ssh_key: None, // SSH key would be loaded elsewhere if needed
        }
    }

    /// Check if the repository exists locally
    pub fn exists(&self) -> bool {
        self.path.join(".git").exists()
    }

    /// Initialize or update the repository
    pub async fn init(&mut self) -> Result<()> {
        if self.exists() {
            self.update().await
        } else {
            self.clone().await
        }
    }

    /// Clone the repository
    pub async fn clone(&mut self) -> Result<()> {
        info!("Cloning repository {} to {}", self.remote_url, self.path.display());
        
        // Create directory if it doesn't exist
        if self.path.exists() {
            warn!("Directory exists but is not a git repository. Creating backup and removing contents.");
            self.backup_directory().await?;
        } else {
            tokio::fs::create_dir_all(&self.path).await
                .context("Failed to create directory for repository")?;
        }
        
        // Build clone command
        let mut cmd = self.build_git_command();
        cmd.args(["clone", "--depth", "1", "-b", &self.branch, &self.remote_url, "."]);
        cmd.current_dir(&self.path);
        
        // Execute clone
        let output = cmd.output().await
            .context("Failed to execute git clone command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git clone failed: {}", stderr));
        }
        
        // Get current commit hash
        self.current_commit = Some(self.get_commit_hash().await?);
        info!("Repository cloned successfully. Current commit: {}", 
              self.current_commit.as_ref().unwrap_or(&"unknown".to_string()));
        
        Ok(())
    }

    /// Update an existing repository
    pub async fn update(&mut self) -> Result<()> {
        debug!("Updating repository at {}", self.path.display());
        
        // Get current commit for potential rollback
        let previous_commit = self.get_commit_hash().await?;
        self.current_commit = Some(previous_commit.clone());
        
        // Check current branch
        let current_branch = self.get_current_branch().await?;
        
        // Switch branch if needed
        if current_branch != self.branch {
            info!("Switching from branch {} to {}", current_branch, self.branch);
            self.switch_branch(&current_branch).await?;
        }
        
        // Fetch the latest changes
        self.fetch().await?;
        
        // Check if there are changes to pull
        let remote_ref = format!("origin/{}", self.branch);
        let remote_commit = self.get_remote_commit_hash(&remote_ref).await?;
        
        if remote_commit != previous_commit {
            // Changes detected
            info!("Changes detected, pulling latest code (current: {}, remote: {})", 
                  previous_commit, remote_commit);
            
            // Check for local changes
            let has_local_changes = self.has_local_changes().await?;
            
            if has_local_changes {
                warn!("Local uncommitted changes detected. Stashing them.");
                self.stash_changes().await?;
            }
            
            // Pull changes
            if let Err(e) = self.pull().await {
                error!("Failed to pull changes: {}", e);
                
                // Check for merge conflicts
                if e.to_string().contains("CONFLICT") || e.to_string().contains("Automatic merge failed") {
                    error!("Merge conflicts detected. Reverting to previous state.");
                    self.reset_hard(&previous_commit).await?;
                }
                
                return Err(e);
            }
            
            // Update current commit
            self.current_commit = Some(remote_commit);
            
            // Apply stashed changes if any
            if has_local_changes {
                info!("Applying stashed changes");
                self.stash_pop().await?;
            }
            
            Ok(())
        } else {
            debug!("No changes detected");
            Ok(())
        }
    }

    /// Check for updates and pull if available
    pub async fn check_for_updates(&mut self) -> Result<bool> {
        debug!("Checking for updates in repository at {}", self.path.display());
        
        // Get current commit hash
        let current_hash = self.get_commit_hash().await?;
        self.current_commit = Some(current_hash.clone());
        
        // Fetch updates
        self.fetch().await?;
        
        // Check if there are changes to pull
        let remote_ref = format!("origin/{}", self.branch);
        let remote_hash = self.get_remote_commit_hash(&remote_ref).await?;
        
        debug!("Current hash: {}, Remote hash: {}", current_hash, remote_hash);
        
        if current_hash != remote_hash {
            // Pull the changes
            self.pull().await?;
            self.current_commit = Some(remote_hash);
            Ok(true) // Changes detected and pulled
        } else {
            Ok(false) // No changes
        }
    }

    /// Revert to a previous commit if validation fails
    pub async fn revert_changes(&mut self) -> Result<()> {
        debug!("Reverting changes in repository at {}", self.path.display());
        
        // Reset to the previous commit
        let mut cmd = self.build_git_command();
        cmd.args(["reset", "--hard", "HEAD@{1}"]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git reset command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git reset failed: {}", stderr));
        }
        
        // Update current commit
        self.current_commit = Some(self.get_commit_hash().await?);
        
        Ok(())
    }

    // ---------- Helper methods ----------

    /// Get the current commit hash
    async fn get_commit_hash(&self) -> Result<String> {
        let mut cmd = self.build_git_command();
        cmd.args(["rev-parse", "HEAD"]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git rev-parse command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git rev-parse failed: {}", stderr));
        }
        
        let hash = String::from_utf8(output.stdout)
            .context("Failed to parse git rev-parse output")?
            .trim()
            .to_string();
        
        Ok(hash)
    }

    /// Get a remote commit hash
    async fn get_remote_commit_hash(&self, remote_ref: &str) -> Result<String> {
        let mut cmd = self.build_git_command();
        cmd.args(["rev-parse", remote_ref]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context(format!("Failed to execute git rev-parse for {}", remote_ref))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git rev-parse for {} failed: {}", remote_ref, stderr));
        }
        
        let hash = String::from_utf8(output.stdout)
            .context("Failed to parse git rev-parse output")?
            .trim()
            .to_string();
        
        Ok(hash)
    }

    /// Get the current branch name
    async fn get_current_branch(&self) -> Result<String> {
        let mut cmd = self.build_git_command();
        cmd.args(["rev-parse", "--abbrev-ref", "HEAD"]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git rev-parse command for branch")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git rev-parse for branch failed: {}", stderr));
        }
        
        let branch = String::from_utf8(output.stdout)
            .context("Failed to parse git branch output")?
            .trim()
            .to_string();
        
        Ok(branch)
    }

    /// Check if there are local uncommitted changes
    async fn has_local_changes(&self) -> Result<bool> {
        let mut cmd = self.build_git_command();
        cmd.args(["status", "--porcelain"]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git status command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git status failed: {}", stderr));
        }
        
        Ok(!output.stdout.is_empty())
    }

    /// Stash local changes
    async fn stash_changes(&self) -> Result<()> {
        let mut cmd = self.build_git_command();
        cmd.args(["stash", "save", "Auto-stash before updating"]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git stash command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Git stash may have failed: {}", stderr);
            // Continue anyway as it might just be that there are no changes to stash
        }
        
        Ok(())
    }

    /// Apply stashed changes
    async fn stash_pop(&self) -> Result<()> {
        let mut cmd = self.build_git_command();
        cmd.args(["stash", "pop"]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git stash pop command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // This could fail if there were conflicts, but we don't want to fail the process
            warn!("Failed to apply stashed changes: {}", stderr);
        }
        
        Ok(())
    }

    /// Fetch from remote
    async fn fetch(&self) -> Result<()> {
        let mut cmd = self.build_git_command();
        cmd.args(["fetch", "origin", &self.branch]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git fetch command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git fetch failed: {}", stderr));
        }
        
        Ok(())
    }

    /// Pull from remote
    async fn pull(&self) -> Result<()> {
        let mut cmd = self.build_git_command();
        cmd.args(["pull", "origin", &self.branch]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git pull command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git pull failed: {}", stderr));
        }
        
        Ok(())
    }

    /// Reset to a specific commit
    async fn reset_hard(&self, commit: &str) -> Result<()> {
        let mut cmd = self.build_git_command();
        cmd.args(["reset", "--hard", commit]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git reset command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git reset failed: {}", stderr));
        }
        
        Ok(())
    }

    /// Switch to a different branch
    async fn switch_branch(&self, current_branch: &str) -> Result<()> {
        // Stash any uncommitted changes
        let has_changes = self.has_local_changes().await?;
        if has_changes {
            warn!("Found uncommitted changes, stashing them before switch");
            self.stash_changes().await?;
        }
        
        // Check if the branch exists locally
        let branch_exists = self.branch_exists_locally(&self.branch).await?;
        
        if branch_exists {
            // Local branch exists, check it out
            debug!("Branch {} exists locally, checking it out", self.branch);
            let mut cmd = self.build_git_command();
            cmd.args(["checkout", &self.branch]);
            cmd.current_dir(&self.path);
            
            let output = cmd.output().await
                .context("Failed to execute git checkout command")?;
            
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow!("Git checkout failed: {}", stderr));
            }
        } else {
            // Check if the branch exists on the remote
            debug!("Branch {} not found locally, checking remote", self.branch);
            self.fetch().await?;
            
            let remote_exists = self.branch_exists_remotely(&self.branch).await?;
            
            if remote_exists {
                // Create a tracking branch
                debug!("Branch {} exists on remote, creating tracking branch", self.branch);
                let mut cmd = self.build_git_command();
                cmd.args(["checkout", "-b", &self.branch, &format!("origin/{}", self.branch)]);
                cmd.current_dir(&self.path);
                
                let output = cmd.output().await
                    .context("Failed to execute git checkout -b command")?;
                
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(anyhow!("Git checkout -b failed: {}", stderr));
                }
            } else {
                return Err(anyhow!("Branch {} not found on remote", self.branch));
            }
        }
        
        // Apply stashed changes if any
        if has_changes {
            debug!("Applying stashed changes after branch switch");
            self.stash_pop().await?;
        }
        
        Ok(())
    }

    /// Check if a branch exists locally
    async fn branch_exists_locally(&self, branch: &str) -> Result<bool> {
        let mut cmd = self.build_git_command();
        cmd.args(["branch", "--list", branch]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git branch command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git branch --list failed: {}", stderr));
        }
        
        Ok(!output.stdout.is_empty())
    }

    /// Check if a branch exists on the remote
    async fn branch_exists_remotely(&self, branch: &str) -> Result<bool> {
        let mut cmd = self.build_git_command();
        cmd.args(["ls-remote", "--heads", "origin", branch]);
        cmd.current_dir(&self.path);
        
        let output = cmd.output().await
            .context("Failed to execute git ls-remote command")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Git ls-remote failed: {}", stderr));
        }
        
        Ok(!output.stdout.is_empty())
    }

    /// Build a git command with proper SSH key handling if needed
    fn build_git_command(&self) -> Command {
        let mut cmd = Command::new("git");
        
        // Configure SSH if a key is provided
        if let Some(key) = &self.ssh_key {
            debug!("Using SSH key for git authentication");
            
            // Create a custom GIT_SSH_COMMAND that uses the key
            // This will be set in a future update when needed
        }
        
        cmd
    }

    /// Create a backup of the directory
    async fn backup_directory(&self) -> Result<()> {
        let backup_path = self.path.with_extension("bak");
        
        // Remove old backup if it exists
        if backup_path.exists() {
            tokio::fs::remove_dir_all(&backup_path).await
                .context("Failed to remove old backup directory")?;
        }
        
        // Move current directory to backup
        tokio::fs::rename(&self.path, &backup_path).await
            .context("Failed to create backup of existing directory")?;
        
        // Create fresh directory
        tokio::fs::create_dir_all(&self.path).await
            .context("Failed to create fresh directory")?;
        
        Ok(())
    }
}

/// Create a temporary file with SSH key content for Git authentication
pub async fn create_ssh_key_file(key_content: &str) -> Result<NamedTempFile> {
    // Create a temporary file for the SSH key
    let temp_file = NamedTempFile::new()
        .context("Failed to create temporary file for SSH key")?;
    
    // Write the key content to the file
    let mut file = File::from_std(temp_file.reopen()?);
    file.write_all(key_content.as_bytes()).await
        .context("Failed to write SSH key to temporary file")?;
    
    // Set correct permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o600))
            .context("Failed to set permissions on SSH key file")?;
    }
    
    Ok(temp_file)
}

/// Main functions for working with service repositories
pub mod service {
    use super::*;
    
    /// Initialize or update a repository for a service
    pub async fn init_repository(service: &ServiceConfig, global: &GlobalSettings) -> Result<()> {
        let mut repo = GitRepo::from_service(service, global);
        repo.init().await
    }
    
    /// Check for updates to a service repository
    pub async fn check_for_updates(service: &ServiceConfig, global: &GlobalSettings) -> Result<bool> {
        let mut repo = GitRepo::from_service(service, global);
        
        if !repo.exists() {
            debug!("Repository does not exist, initializing");
            repo.init().await?;
            return Ok(true); // New repository initialized
        }
        
        repo.check_for_updates().await
    }
    
    /// Revert changes in case of validation failure
    pub async fn revert_changes(service: &ServiceConfig, global: &GlobalSettings) -> Result<()> {
        let mut repo = GitRepo::from_service(service, global);
        
        if !repo.exists() {
            return Err(anyhow!("Cannot revert: repository does not exist"));
        }
        
        repo.revert_changes().await
    }
}