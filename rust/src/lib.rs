mod config;
mod docker_utils;
mod git;
mod nginx;
mod service;
mod utils;

// Re-export main components for easier access
pub use config::{Config, ServiceConfig, GlobalSettings, ServiceType};
pub use docker_utils::ContainerStatus;
pub use git::{GitRepo, service as git_service};
pub use nginx::{check_nginx_status, restart_nginx, check_nginx_logs};
pub use service::{run_validation, restart_service, check_service_status};
pub use utils::fix_permissions;