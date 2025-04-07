#!/bin/sh
set -e

# Print startup message with timestamps
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1"
}

log_debug() {
    if [ "${DEBUG:-false}" = "true" ]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] [DEBUG] $1"
    fi
}

log_warn() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [WARNING] $1"
}

log_error() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ERROR] $1"
}

# Check if required commands are available
check_dependencies() {
    for cmd in jq git docker; do
        if ! command -v $cmd >/dev/null 2>&1; then
            log_error "Required command '$cmd' not found. Please install it first."
            exit 1
        fi
    done
}

# Start execution
log "Starting Config Watcher service"
log "Environment: $APP_ENV"
log "Version: $APP_VERSION"

# Validate required environment variables
if [ -z "$SERVICES_CONFIG" ]; then
    log_error "SERVICES_CONFIG environment variable not set"
    exit 1
fi

if [ -z "$CONFIG_DIR" ]; then
    log_error "CONFIG_DIR environment variable not set"
    exit 1
fi

# Check for required dependencies
check_dependencies

# Validate essential environment variables and create config if needed
if [ ! -f "$SERVICES_CONFIG" ]; then
    # Legacy mode - single repository support
    if [ -n "$REPO_URL" ]; then
        log "Running in legacy mode with single repository"
        
        # Create a services.json file from environment variables
        log "Creating services.json from environment variables"
        cat > "$SERVICES_CONFIG" << EOF
{
  "services": [
    {
      "name": "nginx",
      "container_name": "${NGINX_CONTAINER_NAME:-nginx_app}",
      "repo_url": "$REPO_URL",
      "branch": "${BRANCH:-main}",
      "local_path": "$CONFIG_DIR",
      "use_docker_compose": ${USE_DOCKER_COMPOSE:-false},
      "docker_compose_file": "",
      "restart_command": "",
      "validation_command": "docker exec -t ${NGINX_CONTAINER_NAME:-nginx_app} nginx -t",
      "permissions": {
        "fix": ${FIX_PERMISSIONS:-true},
        "user": "nginx",
        "group": "nginx"
      },
      "monitor_logs": ${MONITOR_LOGS:-true}
    }
  ],
  "global_settings": {
    "watch_interval": ${WATCH_INTERVAL:-60},
    "default_branch": "${BRANCH:-main}",
    "auto_fix": ${AUTO_FIX:-true},
    "fix_permissions": ${FIX_PERMISSIONS:-true},
    "disable_restart": ${DISABLE_RESTART:-false},
    "startup_grace_period": "${STARTUP_GRACE_PERIOD:-30s}"
  }
}
EOF
    else
        log_error "Neither SERVICES_CONFIG file nor REPO_URL environment variable is set"
        log_error "Please either:"
        log_error "1. Provide a services.json file at $SERVICES_CONFIG"
        log_error "2. Set at least REPO_URL for legacy single-service mode"
        exit 1
    fi
fi

# Validate JSON format
if ! jq empty "$SERVICES_CONFIG" 2>/dev/null; then
    log_error "Invalid JSON format in $SERVICES_CONFIG"
    exit 1
fi

# Create required directories
log_debug "Creating config directory: $CONFIG_DIR"
mkdir -p "$CONFIG_DIR"
chmod 775 "$CONFIG_DIR"

# Ensure config dir is writable
if [ ! -w "$CONFIG_DIR" ]; then
    log_error "Config directory $CONFIG_DIR is not writable"
    exit 1
fi

# Set up SSH key if provided
if [ -n "$SSH_PRIVATE_KEY" ]; then
    log "Setting up SSH key for Git authentication"
    
    # Create SSH directory if it doesn't exist
    mkdir -p ~/.ssh
    chmod 700 ~/.ssh
    
    # Write the SSH key to a file
    echo "$SSH_PRIVATE_KEY" > ~/.ssh/id_rsa
    chmod 600 ~/.ssh/id_rsa
    
    # Add known hosts
    for host in github.com gitlab.com bitbucket.org azure.com; do
        if ! grep -q "$host" ~/.ssh/known_hosts 2>/dev/null; then
            if ssh-keyscan -t rsa $host >> ~/.ssh/known_hosts 2>/dev/null; then
                log_debug "Added $host to known hosts"
            else
                log_warn "Failed to add $host to known hosts"
            fi
        fi
    done
    
    # Parse services and convert HTTPS URLs to SSH URLs if needed
    SERVICE_COUNT=$(jq '.services | length' "$SERVICES_CONFIG")
    for i in $(seq 0 $((SERVICE_COUNT-1))); do
        REPO_URL=$(jq -r ".services[$i].repo_url" "$SERVICES_CONFIG")
        SERVICE_NAME=$(jq -r ".services[$i].name" "$SERVICES_CONFIG")
        
        if echo "$REPO_URL" | grep -q "https://github.com/"; then
            SSH_REPO_URL=$(echo "$REPO_URL" | sed 's|https://github.com/|git@github.com:|')
            log "Converting REPO_URL for $SERVICE_NAME from HTTPS to SSH: $SSH_REPO_URL"
            jq ".services[$i].repo_url = \"$SSH_REPO_URL\"" "$SERVICES_CONFIG" > "$SERVICES_CONFIG.tmp" && mv "$SERVICES_CONFIG.tmp" "$SERVICES_CONFIG"
        elif echo "$REPO_URL" | grep -q "https://gitlab.com/"; then
            SSH_REPO_URL=$(echo "$REPO_URL" | sed 's|https://gitlab.com/|git@gitlab.com:|')
            log "Converting REPO_URL for $SERVICE_NAME from HTTPS to SSH: $SSH_REPO_URL"
            jq ".services[$i].repo_url = \"$SSH_REPO_URL\"" "$SERVICES_CONFIG" > "$SERVICES_CONFIG.tmp" && mv "$SERVICES_CONFIG.tmp" "$SERVICES_CONFIG"
        elif echo "$REPO_URL" | grep -q "https://bitbucket.org/"; then
            SSH_REPO_URL=$(echo "$REPO_URL" | sed 's|https://bitbucket.org/|git@bitbucket.org:|')
            log "Converting REPO_URL for $SERVICE_NAME from HTTPS to SSH: $SSH_REPO_URL"
            jq ".services[$i].repo_url = \"$SSH_REPO_URL\"" "$SERVICES_CONFIG" > "$SERVICES_CONFIG.tmp" && mv "$SERVICES_CONFIG.tmp" "$SERVICES_CONFIG"
        fi
    done
fi

# Verify Docker connectivity
log "Testing Docker connectivity"
if ! docker info >/dev/null 2>&1; then
    log_error "Cannot connect to Docker daemon"
    log_error "Make sure /var/run/docker.sock is properly mounted"
    exit 1
fi

# Check for Docker Compose
log "Testing Docker Compose availability"
USE_DOCKER_COMPOSE_GLOBAL=$(jq -r '.global_settings.use_docker_compose // false' "$SERVICES_CONFIG")

if [ "$USE_DOCKER_COMPOSE_GLOBAL" = "true" ]; then
    # Try docker compose V2 first
    if docker compose version >/dev/null 2>&1; then
        log "Using Docker Compose V2"
        DOCKER_COMPOSE_CMD="docker compose"
        export DOCKER_COMPOSE_CMD
    # Fallback to docker-compose (legacy)
    elif docker-compose --version >/dev/null 2>&1; then
        log "Using Docker Compose legacy (docker-compose)"
        DOCKER_COMPOSE_CMD="docker-compose"
        export DOCKER_COMPOSE_CMD
    else
        log_warn "Docker Compose not available, falling back to direct Docker commands"
        # Update the services.json file to disable docker-compose for all services
        jq '.global_settings.use_docker_compose = false' "$SERVICES_CONFIG" > "$SERVICES_CONFIG.tmp" && mv "$SERVICES_CONFIG.tmp" "$SERVICES_CONFIG"
    fi
fi

# Check for stale lock file
if [ -f "/var/run/config_watcher.lock" ]; then
    log "Removing stale lock file"
    rm -f /var/run/config_watcher.lock
fi

# Configure Git globals
git config --global --add safe.directory "$CONFIG_DIR"
git config --global advice.detachedHead false
git config --global pull.rebase false

# Verify containers exist and set up initial repositories
SERVICE_COUNT=$(jq '.services | length' "$SERVICES_CONFIG")
log "Found $SERVICE_COUNT services to monitor"

for i in $(seq 0 $((SERVICE_COUNT-1))); do
    SERVICE_NAME=$(jq -r ".services[$i].name" "$SERVICES_CONFIG")
    CONTAINER_NAME=$(jq -r ".services[$i].container_name" "$SERVICES_CONFIG")
    REPO_URL=$(jq -r ".services[$i].repo_url" "$SERVICES_CONFIG")
    BRANCH=$(jq -r ".services[$i].branch // \"$(jq -r '.global_settings.default_branch // "main"' "$SERVICES_CONFIG")\"" "$SERVICES_CONFIG")
    LOCAL_PATH=$(jq -r ".services[$i].local_path" "$SERVICES_CONFIG")
    USE_DOCKER_COMPOSE=$(jq -r ".services[$i].use_docker_compose // $USE_DOCKER_COMPOSE_GLOBAL" "$SERVICES_CONFIG")
    
    log "Setting up service: $SERVICE_NAME"
    log "- Container: $CONTAINER_NAME"
    log "- Repository: $REPO_URL"
    log "- Branch: $BRANCH"
    
    # Verify container exists if not using Docker Compose
    if [ "$USE_DOCKER_COMPOSE" = "false" ]; then
        log "Checking if container exists: $CONTAINER_NAME"
        if ! docker ps -a --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
            log_warn "Container '$CONTAINER_NAME' does not exist"
            log_warn "The watcher will be unable to restart this service until the container is created"
        fi
    fi
    
    # Create directory for this service if it doesn't exist
    mkdir -p "$LOCAL_PATH"
    
    # Clone repository if it doesn't exist
    if [ ! -d "$LOCAL_PATH/.git" ]; then
        log "Initial clone of repository for $SERVICE_NAME"
        if ! git clone -b "$BRANCH" "$REPO_URL" "$LOCAL_PATH"; then
            log_error "Failed to clone repository for $SERVICE_NAME. Check connectivity and credentials."
            # Continue with next service instead of failing completely
            continue
        fi
    else
        log "Repository for $SERVICE_NAME already exists, updating"
        cd "$LOCAL_PATH" || { log_error "Could not change to $LOCAL_PATH directory"; continue; }
        
        # Fetch updates with error handling
        if ! git fetch origin; then
            log_error "Failed to fetch updates for $SERVICE_NAME"
            continue
        fi
        
        # Checkout branch with error handling
        if ! git checkout "$BRANCH"; then
            log_error "Failed to checkout branch $BRANCH for $SERVICE_NAME"
            continue
        fi
        
        # Pull updates with error handling
        if ! git pull origin "$BRANCH"; then
            log_error "Failed to pull updates for $SERVICE_NAME"
            continue
        fi
    fi
    
    # Fix permissions if configured
    FIX_PERMISSIONS=$(jq -r ".services[$i].permissions.fix // $(jq -r '.global_settings.fix_permissions // true' "$SERVICES_CONFIG")" "$SERVICES_CONFIG")
    if [ "$FIX_PERMISSIONS" = "true" ]; then
        USER=$(jq -r ".services[$i].permissions.user // \"watcher\"" "$SERVICES_CONFIG")
        GROUP=$(jq -r ".services[$i].permissions.group // \"watcher\"" "$SERVICES_CONFIG")
        
        log "Fixing permissions for $SERVICE_NAME: $USER:$GROUP"
        
        # Debug user/group existence
        if ! getent passwd "$USER" >/dev/null; then
            log_warn "User '$USER' does not exist in the container - permissions may fail"
        fi
        
        if ! getent group "$GROUP" >/dev/null; then
            log_warn "Group '$GROUP' does not exist in the container - permissions may fail"
        fi
        
        # Try to fix permissions with better error reporting
        if ! chown -R "$USER:$GROUP" "$LOCAL_PATH" 2>/dev/null; then
            ERROR=$?
            log_warn "Could not fix permissions for $SERVICE_NAME (exit code: $ERROR)"
            log_warn "This may be due to missing user/group or insufficient privileges"
            log_debug "Command attempted: chown -R $USER:$GROUP $LOCAL_PATH"
            
            # Fallback to current user if available
            if [ -n "$USER_ID" ] && [ -n "$GROUP_ID" ]; then
                log "Attempting fallback to numeric IDs: $USER_ID:$GROUP_ID"
                chown -R "$USER_ID:$GROUP_ID" "$LOCAL_PATH" 2>/dev/null || 
                    log_warn "Fallback permission fix also failed"
            fi
        else
            log_debug "Successfully set permissions on $LOCAL_PATH to $USER:$GROUP"
        fi
    fi
done

# Wait for services to be ready if configured
WAIT_PERIOD=$(jq -r '.global_settings.startup_grace_period // "30s"' "$SERVICES_CONFIG")
log "Waiting for $WAIT_PERIOD before starting monitoring..."
sleep "${WAIT_PERIOD%s}"

# Print final configuration summary
log "Configuration Summary:"
log "- Services Config: $SERVICES_CONFIG"
log "- Number of Services: $SERVICE_COUNT"
log "- Watch Interval: $(jq -r '.global_settings.watch_interval // 60' "$SERVICES_CONFIG") seconds"
log "- Default Branch: $(jq -r '.global_settings.default_branch // "main"' "$SERVICES_CONFIG")"
log "- Auto Fix: $(jq -r '.global_settings.auto_fix // true' "$SERVICES_CONFIG")"
log "- Fix Permissions: $(jq -r '.global_settings.fix_permissions // true' "$SERVICES_CONFIG")"

# Export key configurations as environment variables for the watcher to use
export DOCKER_COMPOSE_CMD="${DOCKER_COMPOSE_CMD:-docker compose}"
export CONFIG_WATCHER_VERSION="${APP_VERSION:-1.0.0}"

# Start the watcher service
log "Starting Config Watcher"

# Use exec to replace the current process with the watcher
exec /usr/local/bin/watcher