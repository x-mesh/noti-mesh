use serde::Deserialize;
use std::collections::HashMap;
#[derive(Clone, Deserialize)]
pub struct Config {
    /// Language of the text that the server writes: Slack messages, reports, and CSV headers.
    #[serde(default)]
    pub language: Language,
    #[serde(default)]
    pub timing: Timing,
    #[serde(default)]
    pub admins: HashMap<String, Admin>,
    pub workspace: String,
    pub signing_secret_env: String,
    pub api_url: String,
    pub groups: HashMap<String, Group>,
    pub users: HashMap<String, String>,
    pub sources: HashMap<String, Source>,
    pub routes: Vec<Route>,
    pub default_group: String,
    pub ai: Ai,
    #[serde(default)]
    pub channels: Channels,
    #[serde(default)]
    pub prometheus: Prometheus,
}
#[derive(Clone, Deserialize)]
pub struct Prometheus {
    /// Labels whose values put alerts from `POST /api/v2/alerts` into one incident; `...` means all labels.
    #[serde(default = "default_group_by")]
    pub group_by: Vec<String>,
}
impl Default for Prometheus {
    fn default() -> Self {
        Self {
            group_by: default_group_by(),
        }
    }
}
fn default_group_by() -> Vec<String> {
    vec!["alertname".into()]
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Ko,
    En,
}
impl Language {
    pub fn pick<'a>(self, ko: &'a str, en: &'a str) -> &'a str {
        match self {
            Language::Ko => ko,
            Language::En => en,
        }
    }
}
#[derive(Clone, Deserialize)]
pub struct Admin {
    pub token_env: String,
    pub role: AdminRole,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminRole {
    Admin,
    Viewer,
}
impl AdminRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Viewer => "viewer",
        }
    }
}
#[derive(Clone, Deserialize)]
pub struct Group {
    #[serde(default)]
    pub channel: String,
    #[serde(default = "default_slack_enabled")]
    pub slack_enabled: bool,
    pub members: Vec<String>,
    pub backup: String,
    #[serde(default)]
    pub destinations: Vec<Destination>,
}
fn default_slack_enabled() -> bool {
    true
}
#[derive(Clone, Deserialize, Default)]
pub struct Channels {
    pub email: Option<EmailConfig>,
    pub telegram: Option<TelegramConfig>,
}
#[derive(Clone, Deserialize)]
pub struct EmailConfig {
    pub host: String,
    pub port: u16,
    pub from: String,
    pub security: SmtpSecurity,
    pub username_env: Option<String>,
    pub password_env: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmtpSecurity {
    Tls,
    Starttls,
    Local,
}
#[derive(Clone, Deserialize)]
pub struct TelegramConfig {
    pub token_env: String,
    pub api_base: String,
}
#[derive(Clone, Deserialize)]
pub struct Destination {
    pub channel: Channel,
    pub address: String,
    pub mode: DeliveryMode,
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Email,
    Telegram,
}
impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Telegram => "telegram",
        }
    }
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Fallback,
    Parallel,
    Unacknowledged,
}
impl DeliveryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fallback => "fallback",
            Self::Parallel => "parallel",
            Self::Unacknowledged => "unacknowledged",
        }
    }
}
#[derive(Clone, Deserialize)]
pub struct Source {
    pub token_env: String,
}
#[derive(Clone, Deserialize)]
pub struct Route {
    pub service: String,
    pub environment: String,
    pub group: String,
    pub priority_floor: i16,
}
#[derive(Clone, Deserialize)]
pub struct Ai {
    pub enabled: bool,
    pub model: String,
    pub api_url: String,
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.timing.summary_seconds.is_empty()
            || self.timing.summary_seconds.iter().any(|v| *v <= 0)
            || [
                self.timing.digest_seconds,
                self.timing.p1_ack_seconds,
                self.timing.p2_ack_seconds,
                self.timing.escalation_seconds,
            ]
            .iter()
            .any(|v| *v <= 0)
        {
            return Err("timing intervals must be positive".into());
        }
        if self.prometheus.group_by.is_empty()
            || self
                .prometheus
                .group_by
                .iter()
                .any(|label| label.trim().is_empty())
        {
            return Err("prometheus.group_by needs at least one label name".into());
        }
        if self
            .admins
            .iter()
            .any(|(name, admin)| name.trim().is_empty() || admin.token_env.trim().is_empty())
        {
            return Err("admin name and token environment variable are required".into());
        }
        if !self.groups.contains_key(&self.default_group) || self.sources.is_empty() {
            return Err("default group and sources are required".into());
        }
        for g in self.groups.values() {
            if !self.groups.contains_key(&g.backup)
                || g.slack_enabled && g.members.is_empty()
                || g.slack_enabled && g.channel.is_empty()
                || g.members.iter().any(|u| !self.users.contains_key(u))
            {
                return Err("invalid group or membership".into());
            }
        }
        for group in self.groups.values() {
            if !group.slack_enabled
                && (group
                    .destinations
                    .iter()
                    .any(|d| d.mode == DeliveryMode::Fallback)
                    || !group
                        .destinations
                        .iter()
                        .any(|d| d.mode == DeliveryMode::Parallel))
            {
                return Err("a group without Slack requires a parallel destination and cannot use Slack fallback".into());
            }
            let mut seen = std::collections::HashSet::new();
            for destination in &group.destinations {
                if destination.address.trim().is_empty()
                    || destination.address.len() > 254
                    || !seen.insert((destination.channel, destination.address.clone()))
                {
                    return Err("invalid or duplicate destination".into());
                }
                match destination.channel {
                    Channel::Email => {
                        if self.channels.email.is_none()
                            || destination
                                .address
                                .parse::<lettre::message::Mailbox>()
                                .is_err()
                        {
                            return Err(
                                "email destination requires a valid address and provider".into()
                            );
                        }
                    }
                    Channel::Telegram => {
                        let valid_id = destination.address.parse::<i64>().is_ok()
                            || destination.address.strip_prefix('@').is_some_and(|name| {
                                !name.is_empty()
                                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                            });
                        if self.channels.telegram.is_none() || !valid_id {
                            return Err(
                                "Telegram destination requires a valid chat ID and provider".into(),
                            );
                        }
                    }
                }
            }
        }
        if let Some(email) = &self.channels.email
            && (email.host.trim().is_empty()
                || email.port == 0
                || email.from.parse::<lettre::message::Mailbox>().is_err()
                || email.username_env.is_some() != email.password_env.is_some()
                || email
                    .username_env
                    .as_ref()
                    .is_some_and(|v| v.trim().is_empty())
                || email
                    .password_env
                    .as_ref()
                    .is_some_and(|v| v.trim().is_empty()))
        {
            return Err("invalid email provider".into());
        }
        if let Some(telegram) = &self.channels.telegram {
            let parsed =
                reqwest::Url::parse(&telegram.api_base).map_err(|_| "invalid Telegram API URL")?;
            if telegram.token_env.trim().is_empty()
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err("invalid Telegram provider".into());
            }
        }
        for r in &self.routes {
            if !self.groups.contains_key(&r.group) || !(1..=4).contains(&r.priority_floor) {
                return Err("invalid route".into());
            }
        }
        if self.ai.enabled && self.ai.model.is_empty() {
            return Err("AI model is required when enabled".into());
        }
        Ok(())
    }
    pub fn route(&self, service: &str, environment: &str, severity: i16) -> (&str, i16, bool) {
        match self
            .routes
            .iter()
            .find(|r| r.service == service && r.environment == environment)
        {
            Some(r) => (&r.group, severity.min(r.priority_floor), false),
            None => (&self.default_group, severity, true),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct Timing {
    pub summary_seconds: Vec<i64>,
    pub digest_seconds: i64,
    pub p1_ack_seconds: i64,
    pub p2_ack_seconds: i64,
    pub escalation_seconds: i64,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            summary_seconds: vec![60, 120, 240, 480, 960, 1800],
            digest_seconds: 1800,
            p1_ack_seconds: 300,
            p2_ack_seconds: 900,
            escalation_seconds: 1800,
        }
    }
}
impl Timing {
    pub fn ack(&self, p: i16) -> Option<i64> {
        match p {
            1 => Some(self.p1_ack_seconds),
            2 => Some(self.p2_ack_seconds),
            _ => None,
        }
    }
    pub fn summary(&self, step: i32) -> i64 {
        self.summary_seconds[(step as usize).min(self.summary_seconds.len() - 1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_admin_roles_are_explicit() {
        let input = format!(
            "{}\n[admins.alice]\ntoken_env=\"ALICE_TOKEN\"\nrole=\"viewer\"\n",
            include_str!("../config.example.toml")
        );
        let config: Config = toml::from_str(&input).unwrap();
        config.validate().unwrap();
        assert_eq!(config.admins["alice"].role.as_str(), "viewer");
        assert!(toml::from_str::<Config>(&input.replace("viewer", "invalid")).is_err());
        assert!(toml::from_str::<Config>(&input.replace("role=\"viewer\"\n", "")).is_err());
    }
    #[test]
    fn development_channel_configuration_is_valid() {
        let config: Config = toml::from_str(include_str!("../config.dev.toml")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.groups["development"].destinations.len(), 2);
        assert_eq!(config.groups["development-backup"].destinations.len(), 2);
    }
    #[test]
    fn bounded_backoff() {
        assert_eq!(
            (0..8)
                .map(|step| Timing::default().summary(step))
                .collect::<Vec<_>>(),
            vec![60, 120, 240, 480, 960, 1800, 1800, 1800]
        );
    }
    #[test]
    fn urgent_only_escalation() {
        assert_eq!(Timing::default().ack(1), Some(300));
        assert_eq!(Timing::default().ack(2), Some(900));
        assert_eq!(Timing::default().ack(3), None);
    }
}
