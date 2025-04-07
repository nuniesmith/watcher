#!/bin/sh
# Config Watcher Healthcheck Script
# This script performs various checks to ensure the Config Watcher service is healthy

# Define log formatting function
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1"
}

# Check 1: Verify the watcher process is running
if ! pgrep -f "config-watcher" > /dev/null; then
    log "ERROR: Watcher process is not running"
    exit 1
fi

# Check 2: Verify the lockfile exists and is valid if service has been running for a while
if [ -f "/var/run/config_watcher.lock" ]; then
    PID=$(cat /var/run/config_watcher.lock 2>/dev/null)
    # Check if the PID in the lockfile actually exists
    if [ -n "$PID" ] && ! ps -p "$PID" > /dev/null 2>&1; then
        log "ERROR: Stale lockfile found (PID $PID is not running)"
        exit 1
    fi
elif [ -n "$STARTUP_GRACE_PERIOD" ]; then
    # Get container uptime in seconds
    UPTIME_SECONDS=$(awk '{print int($1)}' /proc/uptime)
    
    # Convert STARTUP_GRACE_PERIOD to seconds (assuming format like "30s", "5m")
    GRACE_VALUE=$(echo "$STARTUP_GRACE_PERIOD" | sed 's/[^0-9]//g')
    GRACE_UNIT=$(echo "$STARTUP_GRACE_PERIOD" | sed 's/[0-9]//g')
    
    case "$GRACE_UNIT" in
        m) GRACE_SECONDS=$((GRACE_VALUE * 60)) ;;
        h) GRACE_SECONDS=$((GRACE_VALUE * 3600)) ;;
        *) GRACE_SECONDS=$GRACE_VALUE ;;
    esac
    
    if [ "$UPTIME_SECONDS" -gt "$GRACE_SECONDS" ]; then
        log "ERROR: Lockfile missing - watcher may have failed to initialize properly"
        exit 1
    fi
fi

# Check 3: Verify services.json exists and is valid
if [ ! -f "$SERVICES_CONFIG" ]; then
    log "ERROR: Services configuration file '$SERVICES_CONFIG' does not exist"
    exit 1
fi

if ! jq empty "$SERVICES_CONFIG" 2>/dev/null; then
    log "ERROR: Services configuration file '$SERVICES_CONFIG' contains invalid JSON"
    exit 1
fi

# Check 4: Verify config directory is accessible and writable
if [ ! -d "$CONFIG_DIR" ]; then
    log "ERROR: Config directory '$CONFIG_DIR' does not exist"
    exit 1
fi

if [ ! -w "$CONFIG_DIR" ]; then
    log "ERROR: Config directory '$CONFIG_DIR' is not writable"
    exit 1
fi

# Check 5: Verify Git is working properly
if ! git --version >/dev/null 2>&1; then
    log "ERROR: Git command not available"
    exit 1
fi

# Check 6: Verify Docker connectivity if we're supposed to control Docker
DISABLE_RESTART=$(jq -r '.global_settings.disable_restart // false' "$SERVICES_CONFIG" 2>/dev/null || echo "false")
USE_DOCKER_COMPOSE=$(jq -r '.global_settings.use_docker_compose // false' "$SERVICES_CONFIG" 2>/dev/null || echo "false")

if [ "$USE_DOCKER_COMPOSE" = "true" ] || [ "$DISABLE_RESTART" != "true" ]; then
    if ! docker info >/dev/null 2>&1; then
        log "ERROR: Cannot connect to Docker daemon. Is the socket properly mounted?"
        exit 1
    fi

    # If using docker-compose, verify it's available
    if [ "$USE_DOCKER_COMPOSE" = "true" ]; then
        if ! docker compose version >/dev/null 2>&1 && ! docker-compose --version >/dev/null 2>&1; then
            log "ERROR: Docker Compose not available"
            exit 1
        fi
    fi
fi

# Check 7: Verify service repositories and containers
if [ "$DISABLE_RESTART" != "true" ]; then
    # Get the number of services
    SERVICE_COUNT=$(jq '.services | length' "$SERVICES_CONFIG" 2>/dev/null || echo "0")
    
    if [ "$SERVICE_COUNT" -gt 0 ]; then
        for i in $(seq 0 $((SERVICE_COUNT-1))); do
            SERVICE_NAME=$(jq -r ".services[$i].name" "$SERVICES_CONFIG" 2>/dev/null || echo "unknown")
            CONTAINER_NAME=$(jq -r ".services[$i].container_name" "$SERVICES_CONFIG" 2>/dev/null || echo "")
            LOCAL_PATH=$(jq -r ".services[$i].local_path" "$SERVICES_CONFIG" 2>/dev/null || echo "")
            SERVICE_DISABLE_RESTART=$(jq -r ".services[$i].disable_restart // $DISABLE_RESTART" "$SERVICES_CONFIG" 2>/dev/null || echo "false")
            
            # Check if service path exists and has a Git repository
            if [ -n "$LOCAL_PATH" ] && [ ! -d "$LOCAL_PATH/.git" ]; then
                log "WARNING: Service '$SERVICE_NAME' missing Git repository at '$LOCAL_PATH'"
                # This is a warning, not an error
            fi
            
            # Check if container exists (if restart is not disabled for this service)
            if [ "$SERVICE_DISABLE_RESTART" != "true" ] && [ -n "$CONTAINER_NAME" ]; then
                if ! docker ps -a --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$" >/dev/null 2>&1; then
                    log "WARNING: Container '$CONTAINER_NAME' for service '$SERVICE_NAME' does not exist"
                    # This is a warning, not an error
                fi
            fi
        done
    else
        log "WARNING: No services defined in configuration"
    fi
fi

# Check 8: Check for memory usage (optional)
MEM_USAGE=$(ps -o rss= -p $(pgrep -f "config-watcher") 2>/dev/null | awk '{sum+=$1} END {print sum/1024}' 2>/dev/null)
if [ -n "$MEM_USAGE" ]; then
    if [ "$(echo "$MEM_USAGE > 500" | bc 2>/dev/null)" = "1" ]; then
        log "WARNING: High memory usage detected: ${MEM_USAGE}MB"
        # This is a warning, not an error
    fi
fi

# All checks passed
log "All health checks passed"
exit 0