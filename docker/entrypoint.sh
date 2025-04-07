#!/bin/sh
set -e

# Print startup message with timestamps
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Starting Config Watcher service"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Environment: $APP_ENV"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Version: $APP_VERSION"

# Validate essential environment variables
if [ ! -f "$SERVICES_CONFIG" ]; then
    # Legacy mode - single repository support
    if [ -n "$REPO_URL" ]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Running in legacy mode with single repository"
        
        # Create a services.json file from environment variables
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Creating services.json from environment variables"
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
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ERROR] Neither SERVICES_CONFIG file nor REPO_URL environment variable is set"
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ERROR] Please either:"
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ERROR] 1. Provide a services.json file at $SERVICES_CONFIG"
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ERROR] 2. Set at least REPO_URL for legacy single-service mode"
        exit 1
    fi
fi

# Validate JSON format
if ! jq empty "$SERVICES_CONFIG" 2>/dev/null; then
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ERROR] Invalid JSON format in $SERVICES_CONFIG"
    exit 1
fi

# Create required directories
mkdir -p "$CONFIG_DIR"
chmod 775 "$CONFIG_DIR"

# Set up SSH key if provided
if [ -n "$SSH_PRIVATE_KEY" ]; then
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Setting up SSH key for Git authentication"
    
    # Create SSH directory if it doesn't exist
    mkdir -p ~/.ssh
    chmod 700 ~/.ssh
    
    # Write the SSH key to a file
    echo "$SSH_PRIVATE_KEY" > ~/.ssh/id_rsa
    chmod 600 ~/.ssh/id_rsa
    
    # Add known hosts
    for host in github.com gitlab.com bitbucket.org azure.com; do
        if ! grep -q "$host" ~/.ssh/known_hosts 2>/dev/null; then
            ssh-keyscan -t rsa $host >> ~/.ssh/known_hosts 2>/dev/null || \
                echo "[$(date '+%Y-%m-%d %H:%M:%S')] [WARNING] Failed to add $host to known hosts"
        fi
    done
    
    # Parse services and convert HTTPS URLs to SSH URLs if needed
    SERVICE_COUNT=$(jq '.services | length' "$SERVICES_CONFIG")
    for i in $(seq 0 $((SERVICE_COUNT-1))); do
        REPO_URL=$(jq -r ".services[$i].repo_url" "$SERVICES_CONFIG")
        SERVICE_NAME=$(jq -r ".services[$i].name" "$SERVICES_CONFIG")
        
        if echo "$REPO_URL" | grep -q "https://github.com/"; then
            SSH_REPO_URL=$(echo "$REPO_URL" | sed 's|https://github.com/|git@github.com:|')
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] Converting REPO_URL for $SERVICE_NAME from HTTPS to SSH: $SSH_REPO_URL"
            jq ".services[$i].repo_url = \"$SSH_REPO_URL\"" "$SERVICES_CONFIG" > "$SERVICES_CONFIG.tmp" && mv "$SERVICES_CONFIG.tmp" "$SERVICES_CONFIG"
        elif echo "$REPO_URL" | grep -q "https://gitlab.com/"; then
            SSH_REPO_URL=$(echo "$REPO_URL" | sed 's|https://gitlab.com/|git@gitlab.com:|')
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] Converting REPO_URL for $SERVICE_NAME from HTTPS to SSH: $SSH_REPO_URL"
            jq ".services[$i].repo_url = \"$SSH_REPO_URL\"" "$SERVICES_CONFIG" > "$SERVICES_CONFIG.tmp" && mv "$SERVICES_CONFIG.tmp" "$SERVICES_CONFIG"
        elif echo "$REPO_URL" | grep -q "https://bitbucket.org/"; then
            SSH_REPO_URL=$(echo "$REPO_URL" | sed 's|https://bitbucket.org/|git@bitbucket.org:|')
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] Converting REPO_URL for $SERVICE_NAME from HTTPS to SSH: $SSH_REPO_URL"
            jq ".services[$i].repo_url = \"$SSH_REPO_URL\"" "$SERVICES_CONFIG" > "$SERVICES_CONFIG.tmp" && mv "$SERVICES_CONFIG.tmp" "$SERVICES_CONFIG"
        fi
    done
fi

# Verify Docker connectivity
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Testing Docker connectivity"
if ! docker info >/dev/null 2>&1; then
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ERROR] Cannot connect to Docker daemon"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ERROR] Make sure /var/run/docker.sock is properly mounted"
    exit 1
fi

# Check for Docker Compose
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Testing Docker Compose availability"
USE_DOCKER_COMPOSE_GLOBAL=$(jq -r '.global_settings.use_docker_compose // false' "$SERVICES_CONFIG")

if [ "$USE_DOCKER_COMPOSE_GLOBAL" = "true" ]; then
    # Try docker compose V2 first
    if docker compose version >/dev/null 2>&1; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Using Docker Compose V2"
        DOCKER_COMPOSE_CMD="docker compose"
    # Fallback to docker-compose (legacy)
    elif docker-compose --version >/dev/null 2>&1; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Using Docker Compose legacy (docker-compose)"
        DOCKER_COMPOSE_CMD="docker-compose"
    else
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] [WARNING] Docker Compose not available, falling back to direct Docker commands"
        # Update the services.json file to disable docker-compose for all services
        jq '.global_settings.use_docker_compose = false' "$SERVICES_CONFIG" > "$SERVICES_CONFIG.tmp" && mv "$SERVICES_CONFIG.tmp" "$SERVICES_CONFIG"
    fi
fi

# Check for stale lock file
if [ -f "/var/run/config_watcher.lock" ]; then
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Removing stale lock file"
    rm -f /var/run/config_watcher.lock
fi

# Configure Git globals
git config --global --add safe.directory "$CONFIG_DIR"
git config --global advice.detachedHead false
git config --global pull.rebase false

# Verify containers exist and set up initial repositories
SERVICE_COUNT=$(jq '.services | length' "$SERVICES_CONFIG")
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Found $SERVICE_COUNT services to monitor"

for i in $(seq 0 $((SERVICE_COUNT-1))); do
    SERVICE_NAME=$(jq -r ".services[$i].name" "$SERVICES_CONFIG")
    CONTAINER_NAME=$(jq -r ".services[$i].container_name" "$SERVICES_CONFIG")
    REPO_URL=$(jq -r ".services[$i].repo_url" "$SERVICES_CONFIG")
    BRANCH=$(jq -r ".services[$i].branch // \"$(jq -r '.global_settings.default_branch // "main"' "$SERVICES_CONFIG")\"" "$SERVICES_CONFIG")
    LOCAL_PATH=$(jq -r ".services[$i].local_path" "$SERVICES_CONFIG")
    USE_DOCKER_COMPOSE=$(jq -r ".services[$i].use_docker_compose // $USE_DOCKER_COMPOSE_GLOBAL" "$SERVICES_CONFIG")
    
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Setting up service: $SERVICE_NAME"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] - Container: $CONTAINER_NAME"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] - Repository: $REPO_URL"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] - Branch: $BRANCH"
    
    # Verify container exists if not using Docker Compose
    if [ "$USE_DOCKER_COMPOSE" = "false" ]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Checking if container exists: $CONTAINER_NAME"
        if ! docker ps -a --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$"; then
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] [WARNING] Container '$CONTAINER_NAME' does not exist"
            echo "[$(date '+%Y-%m-%d %H:%M:%S')] [WARNING] The watcher will be unable to restart this service until the container is created"
        fi
    fi
    
    # Create directory for this service if it doesn't exist
    mkdir -p "$LOCAL_PATH"
    
    # Clone repository if it doesn't exist
    if [ ! -d "$LOCAL_PATH/.git" ]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Initial clone of repository for $SERVICE_NAME"
        git clone -b "$BRANCH" "$REPO_URL" "$LOCAL_PATH"
    else
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Repository for $SERVICE_NAME already exists, updating"
        cd "$LOCAL_PATH"
        git fetch origin
        git checkout "$BRANCH"
        git pull origin "$BRANCH"
    fi
    
    # Fix permissions if configured
    FIX_PERMISSIONS=$(jq -r ".services[$i].permissions.fix // $(jq -r '.global_settings.fix_permissions // true' "$SERVICES_CONFIG")" "$SERVICES_CONFIG")
    if [ "$FIX_PERMISSIONS" = "true" ]; then
        USER=$(jq -r ".services[$i].permissions.user // \"watcher\"" "$SERVICES_CONFIG")
        GROUP=$(jq -r ".services[$i].permissions.group // \"watcher\"" "$SERVICES_CONFIG")
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] Fixing permissions for $SERVICE_NAME: $USER:$GROUP"
        chown -R "$USER:$GROUP" "$LOCAL_PATH" 2>/dev/null || echo "[$(date '+%Y-%m-%d %H:%M:%S')] [WARNING] Could not fix permissions for $SERVICE_NAME"
    fi
done

# Wait for services to be ready if configured
WAIT_PERIOD=$(jq -r '.global_settings.startup_grace_period // "30s"' "$SERVICES_CONFIG")
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Waiting for $WAIT_PERIOD before starting monitoring..."
sleep ${WAIT_PERIOD%s}

# Print final configuration summary
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Configuration Summary:"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] - Services Config: $SERVICES_CONFIG"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] - Number of Services: $SERVICE_COUNT"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] - Watch Interval: $(jq -r '.global_settings.watch_interval // 60' "$SERVICES_CONFIG") seconds"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] - Default Branch: $(jq -r '.global_settings.default_branch // "main"' "$SERVICES_CONFIG")"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] - Auto Fix: $(jq -r '.global_settings.auto_fix // true' "$SERVICES_CONFIG")"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] - Fix Permissions: $(jq -r '.global_settings.fix_permissions // true' "$SERVICES_CONFIG")"

# Start the watcher service
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Starting Config Watcher"
exec /usr/local/bin/config-watcher