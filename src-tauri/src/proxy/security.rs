use crate::proxy::config::{ProxyAuthMode, ProxyConfig, SecurityMonitorConfig};

#[derive(Debug, Clone)]
pub struct ProxySecurityConfig {
    pub auth_mode: ProxyAuthMode,
    pub api_key: String,
    pub admin_password: Option<String>,
    pub allow_lan_access: bool,
    pub tls_network_exposed: bool,
    pub port: u16,
    /// Authorities of the running transports, retained across pending rebinds.
    pub(crate) listener_authorities: Vec<String>,
    pub security_monitor: SecurityMonitorConfig,
}

impl ProxySecurityConfig {
    pub fn from_proxy_config(config: &ProxyConfig) -> Self {
        Self {
            auth_mode: config.auth_mode.clone(),
            api_key: config.api_key.clone(),
            admin_password: config.admin_password.clone(),
            allow_lan_access: config.is_network_exposed(),
            tls_network_exposed: config.tls.enabled && config.is_network_exposed(),
            port: config.port,
            listener_authorities: crate::proxy::middleware::cors::listener_authorities(
                if config.tls.enabled {
                    "127.0.0.1"
                } else {
                    config.get_bind_address()
                },
                config.port,
                config.tls.enabled.then_some((
                    config.get_bind_address(),
                    config.tls.port,
                    config.tls.advertised_host.as_deref(),
                )),
            )
            .unwrap_or_default(),
            security_monitor: config.security_monitor.clone(),
        }
    }

    /// Config edits do not rebind the live listener; retain its exposure until restart.
    pub fn update_from_proxy_config(&mut self, config: &ProxyConfig) {
        let listener_exposed = self.allow_lan_access;
        let tls_exposed = self.tls_network_exposed;
        let port = self.port;
        let authorities = self.listener_authorities.clone();
        *self = Self::from_proxy_config(config);
        self.allow_lan_access = listener_exposed;
        self.tls_network_exposed = tls_exposed;
        self.port = port;
        self.listener_authorities = authorities;
    }

    pub fn effective_auth_mode(&self) -> ProxyAuthMode {
        self.effective_auth_mode_with_tunnel(crate::modules::cloudflared::public_tunnel_active())
    }

    fn effective_auth_mode_with_tunnel(&self, public_tunnel: bool) -> ProxyAuthMode {
        // Public exposure requires authentication for its entire child-process
        // lifetime, even if the user changes Off/Auto while it is running. The
        // stored preference is unchanged and resumes after the tunnel stops.
        if (public_tunnel || self.tls_network_exposed)
            && matches!(self.auth_mode, ProxyAuthMode::Off | ProxyAuthMode::Auto)
        {
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
    fn live_authorities_survive_pending_port_host_and_tls_edits() {
        let mut config = ProxyConfig::default();
        config.bind_address = Some("192.168.1.10".into());
        config.tls.enabled = true;
        config.tls.advertised_host = Some("gateway.home.arpa".into());
        let mut security = ProxySecurityConfig::from_proxy_config(&config);
        let authorities = security.listener_authorities.clone();
        assert!(authorities.contains(&"gateway.home.arpa:8443".into()));
        config.port = 9045;
        config.tls.port = 9443;
        config.tls.advertised_host = Some("pending.home.arpa".into());
        config.bind_address = Some("192.168.1.20".into());
        config.security_monitor.trusted_hosts = vec!["proxy.example:443".into()];
        security.update_from_proxy_config(&config);
        assert_eq!(security.port, 8045);
        assert_eq!(security.listener_authorities, authorities);
        assert_eq!(
            security.security_monitor.trusted_hosts,
            vec!["proxy.example:443"]
        );
        let restarted = ProxySecurityConfig::from_proxy_config(&config);
        assert!(restarted
            .listener_authorities
            .contains(&"pending.home.arpa:9443".into()));
        assert!(!restarted
            .listener_authorities
            .contains(&"gateway.home.arpa:8443".into()));
    }

    #[test]
    fn tls_lan_cannot_disable_auth_until_the_listener_restarts() {
        let mut config = ProxyConfig::default();
        config.allow_lan_access = true;
        config.tls.enabled = true;
        config.auth_mode = ProxyAuthMode::Off;
        let mut security = ProxySecurityConfig::from_proxy_config(&config);
        assert!(matches!(
            security.effective_auth_mode_with_tunnel(false),
            ProxyAuthMode::AllExceptHealth
        ));
        config.tls.enabled = false;
        config.bind_address = Some("127.0.0.1".into());
        security.update_from_proxy_config(&config);
        assert!(matches!(
            security.effective_auth_mode_with_tunnel(false),
            ProxyAuthMode::AllExceptHealth
        ));
        assert!(matches!(
            ProxySecurityConfig::from_proxy_config(&config).effective_auth_mode_with_tunnel(false),
            ProxyAuthMode::Off
        ));
    }

    #[test]
    fn automatic_auth_uses_actual_address_and_survives_pending_rebind() {
        let mut config = ProxyConfig::default();
        config.auth_mode = ProxyAuthMode::Auto;
        config.bind_address = Some("192.168.1.10".into());
        let mut security = ProxySecurityConfig::from_proxy_config(&config);
        assert!(matches!(
            security.effective_auth_mode_with_tunnel(false),
            ProxyAuthMode::AllExceptHealth
        ));
        config.bind_address = Some("::1".into());
        security.update_from_proxy_config(&config);
        assert!(matches!(
            security.effective_auth_mode_with_tunnel(false),
            ProxyAuthMode::AllExceptHealth
        ));
        let restarted = ProxySecurityConfig::from_proxy_config(&config);
        assert!(matches!(
            restarted.effective_auth_mode_with_tunnel(false),
            ProxyAuthMode::Off
        ));
    }

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
            tls_network_exposed: false,
            listener_authorities: crate::proxy::middleware::cors::listener_authorities(
                "127.0.0.1",
                8045,
                None,
            )
            .unwrap(),
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
            tls_network_exposed: false,
            listener_authorities: crate::proxy::middleware::cors::listener_authorities(
                "127.0.0.1",
                8045,
                None,
            )
            .unwrap(),
            port: 8080,
            security_monitor: crate::proxy::config::SecurityMonitorConfig::default(),
        };
        assert!(matches!(
            s.effective_auth_mode(),
            ProxyAuthMode::AllExceptHealth
        ));
    }
}
