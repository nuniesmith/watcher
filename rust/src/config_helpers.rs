use anyhow::{Result, anyhow};
use crate::config::{ServiceConfig, GlobalSettings, nginx};

/// Extension for Config to create various service-specific configs
impl crate::config::Config {
    /// Create a simplified Nginx config for a specific service
    pub fn make_nginx_config(service: &ServiceConfig, global: &GlobalSettings) -> Result<nginx::Config> {
        // Only create an nginx config for Nginx service types
        if service.service_type != crate::config::ServiceType::Nginx {
            return Err(anyhow!("Service is not an Nginx service"));
        }
        
        let compose_dir = service.get_compose_dir(&global.default_compose_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
            
        let compose_file = service.get_compose_file(&global.default_compose_file)
            .unwrap_or_else(|| "docker-compose.yml".to_string());
        
        Ok(nginx::Config {
            nginx_container_name: service.container_name.clone(),
            compose_dir,
            compose_file,
            use_docker_compose: service.use_docker_compose || global.use_docker_compose,
            disable_restart: service.disable_restart || global.disable_restart,
            monitor_logs: service.effective_monitor_logs(global.monitor_logs),
            log_tail_lines: service.log_tail_lines,
            force_rebuild: None,
        })
    }
}