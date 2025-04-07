#!/bin/sh
# Config Watcher Healthcheck Script
# This script performs health checks for the Config Watcher service

# Exit on command failures
set -e

# Define log formatting functions
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1"
}

log_warn() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] WARNING: $1"
}

log_error() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] ERROR: $1"
}

log_debug() {
    if [ "${DEBUG:-false}" = "true" ]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] DEBUG: $1"
    fi
}

# Initialize variables with defaults if not set
SERVICES_CONFIG=${SERVICES_CONFIG:-/app/services.json}
CONFIG_DIR=${CONFIG_DIR:-/app/config}
MAX_MEMORY_MB=${MAX_MEMORY_MB:-500}
HEALTHCHECK_TIMEOUT=${HEALTHCHECK_TIMEOUT:-5}
PID_FILE=${PID_FILE:-/var/run/config_watcher.lock}
LOG_FILE=${LOG_FILE:-/var/log/watcher.log}

# Function to check if a command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Function to check if a process is running
is_process_running() {
    pgrep -f "$1" >/dev/null 2>&1
}

# Function to check if jq query is valid and non-empty
jq_check() {
    local file=$1
    local query=$2
    local default=$3
    
    if [ -f "$file" ]; then
        result=$(jq -r "$query" "$file" 2>/dev/null || echo "$default")
        if [ "$result" = "null" ]; then
            echo "$default"
        else
            echo "$result"
        fi
    else
        echo "$default"
    fi
}

# Run a command with timeout
run_with_timeout() {
    local timeout=$1
    shift
    
    # Use timeout command if available
    if command_exists timeout; then
        timeout "$timeout" "$@"
        return $?
    else
        # Fallback to background execution with kill
        "$@" &
        local pid=$!
        
        # Wait for command to finish or timeout
        local count=0
        while [ $count -lt "$timeout" ] && kill -0 $pid 2>/dev/null; do
            sleep 1
            count=$((count + 1))
        done
        
        # If still running, kill it
        if kill -0 $pid 2>/dev/null; then
            kill $pid
            return 124  # Return timeout exit code
        fi
        
        wait $pid
        return $?
    fi
}

# Start with assuming everything is OK
EXIT_CODE=0
WARNINGS=0

log "Starting health check for Config Watcher service"

# Check 1: Verify critical commands exist
for cmd in jq git docker pgrep ps; do
    if ! command_exists "$cmd"; then
        log_error "Required command '$cmd' not available"
        EXIT_CODE=1
    fi
done

# Check 2: Verify the watcher process is running
if ! is_process_running "watcher"; then
    log_error "Watcher process is not running"
    EXIT_CODE=1
else
    log_debug "Watcher process is running"
    
    # Get the PID of the watcher process
    WATCHER_PID=$(pgrep -f "watcher" | head -1)
    log_debug "Watcher PID: $WATCHER_PID"
    
    # Check for zombie state
    PROCESS_STATE=$(ps -o state= -p "$WATCHER_PID" 2>/dev/null)
    if [ "$PROCESS_STATE" = "Z" ]; then
        log_error "Watcher process is a zombie (state Z)"
        EXIT_CODE=1
    fi
    
    # Check uptime
    if command_exists ps; then
        PROC_START=$(ps -o lstart= -p "$WATCHER_PID" 2>/dev/null)
        if [ -n "$PROC_START" ]; then
            log_debug "Process start time: $PROC_START"
        fi
    fi
fi

# Check 3: Verify the lockfile exists and is valid if service has been running for a while
if [ -f "$PID_FILE" ]; then
    FILE_PID=$(cat "$PID_FILE" 2>/dev/null)
    
    if [ -z "$FILE_PID" ]; then
        log_error "Lockfile exists but is empty"
        EXIT_CODE=1
    elif ! ps -p "$FILE_PID" > /dev/null 2>&1; then
        log_error "Stale lockfile found (PID $FILE_PID is not running)"
        EXIT_CODE=1
    else
        log_debug "Lockfile is valid with PID $FILE_PID"
        
        # Verify the PID in lockfile matches the actual watcher process
        if [ -n "$WATCHER_PID" ] && [ "$FILE_PID" != "$WATCHER_PID" ]; then
            log_warn "PID in lockfile ($FILE_PID) does not match actual watcher process ($WATCHER_PID)"
            WARNINGS=$((WARNINGS + 1))
        fi
    fi
elif [ -n "$STARTUP_GRACE_PERIOD" ]; then
    # Get container uptime in seconds
    if [ -f /proc/uptime ]; then
        UPTIME_SECONDS=$(awk '{print int($1)}' /proc/uptime)
        
        # Convert STARTUP_GRACE_PERIOD to seconds
        GRACE_VALUE=$(echo "$STARTUP_GRACE_PERIOD" | sed 's/[^0-9]//g')
        GRACE_UNIT=$(echo "$STARTUP_GRACE_PERIOD" | sed 's/[0-9]//g')
        
        case "$GRACE_UNIT" in
            m) GRACE_SECONDS=$((GRACE_VALUE * 60)) ;;
            h) GRACE_SECONDS=$((GRACE_VALUE * 3600)) ;;
            *) GRACE_SECONDS=$GRACE_VALUE ;;
        esac
        
        log_debug "Uptime: $UPTIME_SECONDS seconds, Grace period: $GRACE_SECONDS seconds"
        
        if [ "$UPTIME_SECONDS" -gt "$GRACE_SECONDS" ]; then
            log_error "Lockfile missing after grace period - watcher may have failed to initialize"
            EXIT_CODE=1
        else
            log_debug "Within grace period, lockfile not required yet"
        fi
    else
        log_warn "Cannot determine uptime - skipping grace period check"
        WARNINGS=$((WARNINGS + 1))
    fi
else
    log_error "Lockfile does not exist and no grace period defined"
    EXIT_CODE=1
fi

# Check 4: Verify services.json exists and is valid
if [ ! -f "$SERVICES_CONFIG" ]; then
    log_error "Services configuration file '$SERVICES_CONFIG' does not exist"
    EXIT_CODE=1
else
    log_debug "Services config file exists: $SERVICES_CONFIG"
    
    # Check if file is empty
    if [ ! -s "$SERVICES_CONFIG" ]; then
        log_error "Services configuration file is empty"
        EXIT_CODE=1
    fi
    
    # Validate JSON syntax
    if ! jq empty "$SERVICES_CONFIG" 2>/dev/null; then
        log_error "Services configuration file contains invalid JSON"
        EXIT_CODE=1
    else
        log_debug "Services JSON syntax is valid"
    fi
    
    # Check if any services are defined
    SERVICE_COUNT=$(jq '.services | length' "$SERVICES_CONFIG" 2>/dev/null || echo "0")
    if [ "$SERVICE_COUNT" -eq 0 ]; then
        log_warn "No services defined in configuration"
        WARNINGS=$((WARNINGS + 1))
    else
        log_debug "Found $SERVICE_COUNT service(s) in configuration"
    fi
fi

# Check 5: Verify config directory is accessible and writable
if [ ! -d "$CONFIG_DIR" ]; then
    log_error "Config directory '$CONFIG_DIR' does not exist"
    EXIT_CODE=1
else
    log_debug "Config directory exists: $CONFIG_DIR"
    
    if [ ! -w "$CONFIG_DIR" ]; then
        log_error "Config directory '$CONFIG_DIR' is not writable"
        EXIT_CODE=1
    else
        log_debug "Config directory is writable"
    fi
    
    # Check for disk space issues
    if command_exists df; then
        DISK_USAGE=$(df -h "$CONFIG_DIR" | awk 'NR==2 {print $5}' | sed 's/%//')
        if [ -n "$DISK_USAGE" ] && [ "$DISK_USAGE" -gt 90 ]; then
            log_warn "Disk usage is high: ${DISK_USAGE}%"
            WARNINGS=$((WARNINGS + 1))
        else
            log_debug "Disk usage: ${DISK_USAGE:-unknown}%"
        fi
    fi
fi

# Check 6: Verify Git is working properly
if ! command_exists git; then
    log_error "Git command not available"
    EXIT_CODE=1
else
    log_debug "Git command available: $(git --version | head -1)"
    
    # Test Git functionality
    if [ -d "$CONFIG_DIR" ]; then
        if ! run_with_timeout "$HEALTHCHECK_TIMEOUT" git --git-dir="$CONFIG_DIR/.git" status >/dev/null 2>&1; then
            log_warn "Git operations may be failing in config directory"
            WARNINGS=$((WARNINGS + 1))
        fi
    fi
fi

# Check 7: Verify Docker connectivity if we're supposed to control Docker
DISABLE_RESTART=$(jq_check "$SERVICES_CONFIG" '.global_settings.disable_restart // false' "false")
USE_DOCKER_COMPOSE=$(jq_check "$SERVICES_CONFIG" '.global_settings.use_docker_compose // false' "false")

if [ "$USE_DOCKER_COMPOSE" = "true" ] || [ "$DISABLE_RESTART" != "true" ]; then
    if ! run_with_timeout "$HEALTHCHECK_TIMEOUT" docker info >/dev/null 2>&1; then
        log_error "Cannot connect to Docker daemon. Is the socket properly mounted?"
        EXIT_CODE=1
    else
        log_debug "Docker connectivity verified"
        
        # If using docker-compose, verify it's available
        if [ "$USE_DOCKER_COMPOSE" = "true" ]; then
            if ! run_with_timeout "$HEALTHCHECK_TIMEOUT" docker compose version >/dev/null 2>&1 && 
               ! run_with_timeout "$HEALTHCHECK_TIMEOUT" docker-compose --version >/dev/null 2>&1; then
                log_error "Docker Compose not available"
                EXIT_CODE=1
            else
                log_debug "Docker Compose is available"
            fi
        fi
    fi
fi

# Check 8: Verify service repositories and containers
if [ "$DISABLE_RESTART" != "true" ] && [ -f "$SERVICES_CONFIG" ]; then
    # Get the number of services
    SERVICE_COUNT=$(jq '.services | length' "$SERVICES_CONFIG" 2>/dev/null || echo "0")
    
    if [ "$SERVICE_COUNT" -gt 0 ]; then
        for i in $(seq 0 $((SERVICE_COUNT-1))); do
            SERVICE_NAME=$(jq_check "$SERVICES_CONFIG" ".services[$i].name" "unknown")
            CONTAINER_NAME=$(jq_check "$SERVICES_CONFIG" ".services[$i].container_name" "")
            LOCAL_PATH=$(jq_check "$SERVICES_CONFIG" ".services[$i].local_path" "")
            SERVICE_DISABLE_RESTART=$(jq_check "$SERVICES_CONFIG" ".services[$i].disable_restart // $DISABLE_RESTART" "false")
            
            log_debug "Checking service: $SERVICE_NAME (Container: $CONTAINER_NAME, Path: $LOCAL_PATH)"
            
            # Check if service path exists and has a Git repository
            if [ -n "$LOCAL_PATH" ]; then
                if [ ! -d "$LOCAL_PATH" ]; then
                    log_warn "Service '$SERVICE_NAME' directory doesn't exist: '$LOCAL_PATH'"
                    WARNINGS=$((WARNINGS + 1))
                elif [ ! -d "$LOCAL_PATH/.git" ]; then
                    log_warn "Service '$SERVICE_NAME' missing Git repository at '$LOCAL_PATH'"
                    WARNINGS=$((WARNINGS + 1))
                else
                    log_debug "Service '$SERVICE_NAME' has valid Git repository"
                fi
            fi
            
            # Check if container exists (if restart is not disabled for this service)
            if [ "$SERVICE_DISABLE_RESTART" != "true" ] && [ -n "$CONTAINER_NAME" ]; then
                if ! run_with_timeout "$HEALTHCHECK_TIMEOUT" docker ps -a --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$" >/dev/null 2>&1; then
                    log_warn "Container '$CONTAINER_NAME' for service '$SERVICE_NAME' does not exist"
                    WARNINGS=$((WARNINGS + 1))
                else
                    # Check if container is running
                    if ! run_with_timeout "$HEALTHCHECK_TIMEOUT" docker ps --format '{{.Names}}' | grep -q "^${CONTAINER_NAME}$" >/dev/null 2>&1; then
                        log_warn "Container '$CONTAINER_NAME' for service '$SERVICE_NAME' exists but is not running"
                        WARNINGS=$((WARNINGS + 1))
                    else
                        log_debug "Container '$CONTAINER_NAME' is running"
                    fi
                fi
            fi
        done
    fi
fi

# Check 9: Check for memory usage
if [ -n "$WATCHER_PID" ] && command_exists ps; then
    MEM_USAGE=$(ps -o rss= -p "$WATCHER_PID" 2>/dev/null | awk '{print int($1/1024)}' 2>/dev/null)
    
    if [ -n "$MEM_USAGE" ]; then
        log_debug "Memory usage: ${MEM_USAGE}MB"
        
        if [ "$MEM_USAGE" -gt "$MAX_MEMORY_MB" ]; then
            log_warn "High memory usage detected: ${MEM_USAGE}MB (threshold: ${MAX_MEMORY_MB}MB)"
            WARNINGS=$((WARNINGS + 1))
        fi
    else
        log_warn "Unable to determine memory usage"
        WARNINGS=$((WARNINGS + 1))
    fi
fi

# Check 10: Look for errors in logs if log file exists
if [ -f "$LOG_FILE" ]; then
    log_debug "Checking log file: $LOG_FILE"
    
    # Check for recent errors in log file (last 100 lines)
    ERROR_COUNT=$(tail -n 100 "$LOG_FILE" 2>/dev/null | grep -c "\[ERROR\]" || echo "0")
    
    if [ "$ERROR_COUNT" -gt 0 ]; then
        log_warn "Found $ERROR_COUNT recent error(s) in log file"
        WARNINGS=$((WARNINGS + 1))
        
        # Show the last error for context
        LAST_ERROR=$(tail -n 100 "$LOG_FILE" 2>/dev/null | grep "\[ERROR\]" | tail -1)
        if [ -n "$LAST_ERROR" ]; then
            log_warn "Last error: $LAST_ERROR"
        fi
    else
        log_debug "No recent errors found in log file"
    fi
else
    log_debug "Log file not found at $LOG_FILE - skipping log check"
fi

# Final health assessment
if [ "$EXIT_CODE" -ne 0 ]; then
    log_error "Health check failed with $EXIT_CODE critical issue(s) and $WARNINGS warning(s)"
    exit "$EXIT_CODE"
elif [ "$WARNINGS" -gt 0 ]; then
    log "Health check passed with $WARNINGS warning(s)"
    exit 0
else
    log "All health checks passed successfully"
    exit 0
fi