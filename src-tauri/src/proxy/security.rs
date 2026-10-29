use crate::proxy::config::{ProxyAuthMode, ProxyConfig, SecurityMonitorConfig};

#[derive(Debug, Clone)]
pub struct ProxySecurityConfig {
    pub auth_mode: ProxyAuthMode,
    pub api_key: String,
    pub admin_password: Option<String>,
    pub allow_lan_access: bool,
    pub port: u16,
    pub security_monitor: SecurityMonitorConfig,
}

impl ProxySecurityConfig {
    pub fn from_proxy_config(config: &ProxyConfig) -> Self {
        Self {
            auth_mode: config.auth_mode.clone(),
            api_key: config.api_key.clone(),
            admin_password: config.admin_password.clone(),
            allow_lan_access: config.allow_lan_access,
            port: config.port,
            security_monitor: config.security_monitor.clone(),
        }
    }

    pub fn effective_auth_mode(&self) -> ProxyAuthMode {
        self.effective_auth_mode_with_tunnel(crate::modules::cloudflared::public_tunnel_active())
    }

    fn effective_auth_mode_with_tunnel(&self, public_tunnel: bool) -> ProxyAuthMode {
        // Public exposure requires authentication for its entire child-process
        // lifetime, even if the user changes Off/Auto while it is running. The
        // stored preference is unchanged and resumes after the tunnel stops.
        if public_tunnel && matches!(self.auth_mode, ProxyAuthMode::Off | ProxyAuthMode::Auto) {
            return ProxyAuthMode::AllExceptHealth;
        }
        match self.auth_mode {
            ProxyAuthMode::Auto => {
                if self.allow_lan_access {
                    ProxyAuthMode::AllExceptHealth
                } else {
                    ProxyAuthMode::Off
                }
            }
            ref other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_public_tunnels_require_auth_without_rewriting_the_preference() {
        let mut security = ProxySecurityConfig::from_proxy_config(&Default::default());
        for mode in [ProxyAuthMode::Off, ProxyAuthMode::Auto] {
            security.auth_mode = mode;
            assert!(matches!(
                security.effective_auth_mode_with_tunnel(true),
                ProxyAuthMode::AllExceptHealth
            ));
            assert!(matches!(
                security.effective_auth_mode_with_tunnel(false),
                ProxyAuthMode::Off
            ));
        }
        security.auth_mode = ProxyAuthMode::Strict;
        assert!(matches!(
            security.effective_auth_mode_with_tunnel(true),
            ProxyAuthMode::Strict
        ));
    }

    #[test]
    fn auto_mode_resolves_off_for_local_only() {
        let s = ProxySecurityConfig {
            auth_mode: ProxyAuthMode::Auto,
            api_key: "sk-test".to_string(),
            admin_password: None,
            allow_lan_access: false,
            port: 8080,
            security_monitor: crate::proxy::config::SecurityMonitorConfig::default(),
        };
        assert!(matches!(s.effective_auth_mode(), ProxyAuthMode::Off));
    }

    #[test]
    fn auto_mode_resolves_all_except_health_for_lan() {
        let s = ProxySecurityConfig {
            auth_mode: ProxyAuthMode::Auto,
            api_key: "sk-test".to_string(),
            admin_password: None,
            allow_lan_access: true,
            port: 8080,
            security_monitor: crate::proxy::config::SecurityMonitorConfig::default(),
        };
        assert!(matches!(
            s.effective_auth_mode(),
            ProxyAuthMode::AllExceptHealth
        ));
    }
}
