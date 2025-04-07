#!/bin/bash

DOCKERHUB_USERNAME="nuniesmith"
DOCKERHUB_REPOSITORY="nginx"

# --- System Paths ---
NGINX_DOCKERFILE_PATH=./docker/nginx/Dockerfile

# List of services and their Dockerfile paths
declare -A services=(
  ["nginx"]="${NGINX_DOCKERFILE_PATH}"
)

# Loop through the services and build/push each one
for service in "${!services[@]}"; do
  dockerfile=${services[$service]}
  service_sanitized="$(echo "$service" | tr ' ' '_')"
  
  echo "----------------------------------------"
  echo "Building and pushing image for service: $service"
  echo "Using Dockerfile: $dockerfile"
  
  # Build the Docker image
  docker build \
    --build-arg PYTHONPATH="$PYTHONPATH" \
    -t "$DOCKERHUB_USERNAME/$DOCKERHUB_REPOSITORY:$service_sanitized" \
    -f "$dockerfile" .
    
  if [ $? -eq 0 ]; then
    echo "Successfully built $service image."
    
    # Push the Docker image to Docker Hub
    docker push "$DOCKERHUB_USERNAME/$DOCKERHUB_REPOSITORY:$service_sanitized"
    
    if [ $? -eq 0 ]; then
      echo "Successfully pushed $service image to Docker Hub."
    else
      echo "Failed to push $service image to Docker Hub."
    fi
  else
    echo "Failed to build $service image. Skipping push."
  fi
  
  echo "----------------------------------------"
done