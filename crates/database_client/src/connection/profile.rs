use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    #[serde(default)]
    pub ssl_mode: SslMode,
    #[serde(default)]
    pub read_only: bool,
}

impl ConnectionProfile {
    /// Stable key used to store/retrieve the password in the OS keychain.
    pub fn keychain_url(&self) -> String {
        format!(
            "postgres://{}@{}:{}/{}",
            self.user, self.host, self.port, self.database
        )
    }

    /// Parse a Postgres URL into a `ConnectionProfile`. Returns `None` if the URL
    /// cannot be parsed or is missing required fields (host, user).
    /// The `id` field is left empty; callers may assign one as needed.
    pub fn from_url(url: &str) -> Option<ConnectionProfile> {
        use std::str::FromStr as _;
        let cfg = tokio_postgres::Config::from_str(url).ok()?;

        let host = cfg.get_hosts().first().and_then(|h| match h {
            tokio_postgres::config::Host::Tcp(s) => Some(s.clone()),
            #[cfg(unix)]
            tokio_postgres::config::Host::Unix(_) => None,
        })?;

        let port = *cfg.get_ports().first().unwrap_or(&5432);
        let database = cfg.get_dbname().unwrap_or("postgres").to_string();
        let user = cfg.get_user()?.to_string();

        let ssl_mode = match cfg.get_ssl_mode() {
            tokio_postgres::config::SslMode::Disable => SslMode::Disable,
            tokio_postgres::config::SslMode::Require => SslMode::Require,
            _ => SslMode::Prefer,
        };

        Some(ConnectionProfile {
            id: String::new(),
            name: format!("{user}@{host}/{database}"),
            host,
            port,
            database,
            user,
            ssl_mode,
            read_only: false,
        })
    }

    /// Build a tokio-postgres connection config. TLS negotiation is decided by
    /// the caller based on `ssl_mode`; this only fills the connection fields.
    pub fn pg_config(&self, password: Option<&str>) -> tokio_postgres::Config {
        let mut cfg = tokio_postgres::Config::new();
        let pg_ssl = match self.ssl_mode {
            SslMode::Disable => tokio_postgres::config::SslMode::Disable,
            SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
            SslMode::Require => tokio_postgres::config::SslMode::Require,
        };
        cfg.host(&self.host)
            .port(self.port)
            .dbname(&self.database)
            .user(&self.user)
            .application_name("Zedium")
            .ssl_mode(pg_ssl);
        if let Some(pw) = password {
            cfg.password(pw);
        }
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ConnectionProfile {
        ConnectionProfile {
            id: "p1".into(),
            name: "Local".into(),
            host: "localhost".into(),
            port: 5432,
            database: "mydb".into(),
            user: "alice".into(),
            ssl_mode: SslMode::Prefer,
            read_only: false,
        }
    }

    #[test]
    fn read_only_defaults_false_for_old_json() {
        // Old profile JSON without the field must load as read_only = false.
        let json = r#"{"id":"p1","name":"Local","host":"localhost","port":5432,
            "database":"mydb","user":"alice","ssl_mode":"prefer"}"#;
        let profile: ConnectionProfile = serde_json::from_str(json).unwrap();
        assert!(!profile.read_only);
    }

    #[test]
    fn read_only_round_trips_when_set() {
        let mut profile = sample();
        profile.read_only = true;
        let json = serde_json::to_string(&profile).unwrap();
        let back: ConnectionProfile = serde_json::from_str(&json).unwrap();
        assert!(back.read_only);
        assert_eq!(profile, back);
    }

    #[test]
    fn serde_round_trip() {
        let p = sample();
        let json = serde_json::to_string(&p).unwrap();
        let back: ConnectionProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn ssl_mode_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&SslMode::Require).unwrap(),
            "\"require\""
        );
    }

    #[test]
    fn keychain_url_is_stable_and_unique() {
        let p = sample();
        assert_eq!(p.keychain_url(), "postgres://alice@localhost:5432/mydb");
    }

    #[test]
    fn pg_config_sets_fields() {
        let p = sample();
        let cfg = p.pg_config(Some("secret"));
        assert_eq!(cfg.get_hosts().len(), 1);
        assert_eq!(cfg.get_ports(), &[5432]);
        assert_eq!(cfg.get_dbname(), Some("mydb"));
        assert_eq!(cfg.get_user(), Some("alice"));
    }

    #[test]
    fn pg_config_sets_ssl_mode() {
        let mut p = sample();
        p.ssl_mode = SslMode::Require;
        assert_eq!(
            p.pg_config(None).get_ssl_mode(),
            tokio_postgres::config::SslMode::Require
        );
        p.ssl_mode = SslMode::Disable;
        assert_eq!(
            p.pg_config(None).get_ssl_mode(),
            tokio_postgres::config::SslMode::Disable
        );
    }
}
