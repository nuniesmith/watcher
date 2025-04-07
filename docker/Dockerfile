# -----------------------------
# Dockerfile for Generic Config Watcher
# -----------------------------
# -----------------------------
# Stage 1: Rust Config Watcher Builder
# -----------------------------
FROM rust:alpine AS watcher-builder
# Install build dependencies with proper version pinning
RUN apk add --no-cache \
    build-base \
    openssl-dev \
    pkgconfig \
    git
# Set up for better dependency caching
WORKDIR /app
# Copy only the manifests first for layer caching
COPY ./rust/Cargo.toml ./rust/Cargo.lock* ./
RUN mkdir -p src && \
    echo 'fn main() { println!("Placeholder"); }' > src/main.rs && \
    cargo build --release && \
    rm -rf src
# Now copy actual source code and rebuild (faster with cached dependencies)
COPY ./rust/src ./src
RUN cargo build --release
# Copy the compiled binary to a specific location
COPY ./services.json /app/services.json
# Create healthcheck and entrypoint scripts
COPY ./docker/healthcheck.sh /app/healthcheck.sh
COPY ./docker/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/healthcheck.sh /app/entrypoint.sh

# -----------------------------
# Stage 2: Config Watcher Runtime - Secured
# -----------------------------
FROM alpine:latest
# Runtime arguments with defaults
ARG APP_VERSION=1.0.0
ARG APP_ENV=production
ARG BUILD_DATE="unknown"
ARG DOCKER_GID=986
# Create a non-root user for better security
RUN addgroup -S watcher && \
    adduser -S -G watcher watcher
# Install required packages for runtime - with proper version pinning
RUN apk update && \
    apk add --no-cache \
    git \
    curl \
    bash \
    openssh-client \
    docker-cli \
    docker-compose \
    libgcc \
    ca-certificates \
    shadow \
    jq
# Set up docker group with same GID as host, handling potential GID conflicts
RUN if getent group ${DOCKER_GID}; then \
        existing_group=$(getent group ${DOCKER_GID} | cut -d: -f1); \
        echo "Warning: GID ${DOCKER_GID} already exists as group '${existing_group}'"; \
        if [ "${existing_group}" != "docker" ]; then \
            echo "Creating docker group with system-assigned GID and adding watcher user"; \
            addgroup docker && adduser watcher docker; \
            echo "Warning: Using system-assigned GID for docker group instead of ${DOCKER_GID}"; \
            echo "This might cause permission issues with the host's Docker socket"; \
        else \
            echo "Docker group already exists with GID ${DOCKER_GID}, adding watcher user to it"; \
            adduser watcher docker; \
        fi; \
    else \
        echo "Creating docker group with GID ${DOCKER_GID}"; \
        addgroup -g ${DOCKER_GID} docker && adduser watcher docker; \
    fi
# Create app directory and config directory with proper permissions
WORKDIR /app
ENV CONFIG_DIR="/app/config"
RUN mkdir -p ${CONFIG_DIR} /var/run && \
    chown -R watcher:watcher /app && \
    chmod 775 ${CONFIG_DIR} && \
    chmod 1777 /var/run
# Set up SSH directory for watcher user
RUN mkdir -p /home/watcher/.ssh && \
    chown -R watcher:watcher /home/watcher && \
    chmod 700 /home/watcher/.ssh
# Set proper Git configuration for safer Git operations
RUN git config --global --add safe.directory ${CONFIG_DIR} && \
    git config --global advice.detachedHead false && \
    git config --global pull.rebase false
# Copy the compiled Rust application, healthcheck, and entrypoint scripts
COPY --from=watcher-builder /app/target/release/config-watcher /usr/local/bin/
COPY --from=watcher-builder /app/healthcheck.sh /usr/local/bin/healthcheck.sh
COPY --from=watcher-builder /app/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY --from=watcher-builder /app/services.json /app/services.json
# Make sure scripts are executable and have correct ownership
RUN chmod +x /usr/local/bin/config-watcher \
    /usr/local/bin/healthcheck.sh \
    /usr/local/bin/entrypoint.sh && \
    chown watcher:watcher /usr/local/bin/config-watcher
# Define default environment variables
ENV SERVICES_CONFIG="/app/services.json" \
    WATCH_INTERVAL=60 \
    DEFAULT_BRANCH="main" \
    USE_DOCKER_COMPOSE="false" \
    AUTO_FIX="true" \
    FIX_PERMISSIONS="true" \
    MONITOR_LOGS="true" \
    DISABLE_RESTART="false" \
    STARTUP_GRACE_PERIOD="30s"
# Label the image with metadata
LABEL org.opencontainers.image.title="config-watcher" \
    org.opencontainers.image.version="${APP_VERSION}" \
    org.opencontainers.image.description="Config watcher service for monitoring and updating configurations from Git" \
    org.opencontainers.image.created="${BUILD_DATE}" \
    org.opencontainers.image.environment="${APP_ENV}" \
    org.opencontainers.image.vendor="nuniesmith" \
    org.opencontainers.image.url="https://github.com/nuniesmith/watcher"
# Switch to non-root user for better security
USER watcher
# Health Check with optimized parameters
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/healthcheck.sh"]
# Set entrypoint
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]