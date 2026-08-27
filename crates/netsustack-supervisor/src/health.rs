use std::{net::SocketAddr, time::Duration};

use netsustack_domain::ServerConfig;
use reqwest::{Client, Url};
use thiserror::Error;
use tokio::{net::TcpStream, time::timeout};

const TCP_TIMEOUT: Duration = Duration::from_secs(2);
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum HealthUrlError {
    #[error("invalid health URL {url:?}: {reason}")]
    Invalid { url: String, reason: String },
}

#[derive(Debug, Clone)]
pub struct HealthChecker {
    client: Option<Client>,
    tcp_timeout: Duration,
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self::new(TCP_TIMEOUT, HTTP_TIMEOUT)
    }
}

impl HealthChecker {
    pub fn new(tcp_timeout: Duration, http_timeout: Duration) -> Self {
        let client = Client::builder()
            .timeout(http_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build();
        Self::from_client_result(tcp_timeout, client)
    }

    fn from_client_result<E>(tcp_timeout: Duration, client: Result<Client, E>) -> Self {
        let client = client.ok();
        Self {
            client,
            tcp_timeout,
        }
    }

    pub async fn check(&self, config: &ServerConfig) -> bool {
        if config
            .health_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
        {
            let Ok(Some(url)) = resolved_health_url(config) else {
                return false;
            };
            return self.http_healthy(url, config.health_status).await;
        }

        match config.port {
            Some(port) => self.tcp_healthy(port).await,
            None => true,
        }
    }

    async fn tcp_healthy(&self, port: u16) -> bool {
        timeout(self.tcp_timeout, async move {
            let mut addresses = vec![
                SocketAddr::from(([127, 0, 0, 1], port)),
                SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port)),
            ];
            if let Ok(resolved) = tokio::net::lookup_host(("localhost", port)).await {
                addresses.extend(resolved);
            }
            addresses.sort_unstable();
            addresses.dedup();
            let mut attempts = tokio::task::JoinSet::new();
            for address in addresses {
                attempts.spawn(TcpStream::connect(address));
            }
            while let Some(attempt) = attempts.join_next().await {
                if matches!(attempt, Ok(Ok(_))) {
                    attempts.abort_all();
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false)
    }

    async fn http_healthy(&self, url: Url, expected: Option<u16>) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        let Ok(response) = client
            .get(url)
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .await
        else {
            return false;
        };
        let status = response.status().as_u16();
        expected.map_or((200..=399).contains(&status), |value| value == status)
    }
}

pub fn resolved_health_url(config: &ServerConfig) -> Result<Option<Url>, HealthUrlError> {
    let Some(raw) = config.health_url.as_deref().map(str::trim) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }

    match Url::parse(raw) {
        Ok(url) => validate_http_url(raw, url).map(Some),
        Err(_) if !has_uri_scheme(raw) => {
            let port = config.port.ok_or_else(|| HealthUrlError::Invalid {
                url: raw.to_owned(),
                reason: "relative URL requires a configured port".into(),
            })?;
            let base = Url::parse(&format!("http://localhost:{port}/")).map_err(|error| {
                HealthUrlError::Invalid {
                    url: raw.to_owned(),
                    reason: error.to_string(),
                }
            })?;
            base.join(raw)
                .map_err(|error| HealthUrlError::Invalid {
                    url: raw.to_owned(),
                    reason: error.to_string(),
                })
                .and_then(|url| validate_http_url(raw, url))
                .map(Some)
        }
        Err(error) => Err(HealthUrlError::Invalid {
            url: raw.to_owned(),
            reason: error.to_string(),
        }),
    }
}

fn validate_http_url(raw: &str, url: Url) -> Result<Url, HealthUrlError> {
    if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() {
        Ok(url)
    } else {
        Err(HealthUrlError::Invalid {
            url: raw.to_owned(),
            reason: "expected an absolute HTTP or HTTPS URL with a host".into(),
        })
    }
}

fn has_uri_scheme(raw: &str) -> bool {
    let Some((scheme, _)) = raw.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.starts_with(|character: char| character.is_ascii_alphabetic())
        && scheme.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use netsustack_domain::ServerConfig;

    use super::HealthChecker;

    #[tokio::test]
    async fn client_construction_error_fails_http_health_closed_without_panicking() {
        let checker = HealthChecker::from_client_result::<&str>(
            Duration::from_secs(2),
            Err("injected client builder failure"),
        );
        let config = ServerConfig {
            id: "srv_health_client_error".into(),
            name: "health client error".into(),
            command: "fixture".into(),
            port: None,
            directory: None,
            env: HashMap::new(),
            health_url: Some("http://127.0.0.1:1/health".into()),
            health_status: None,
            auto_restart: false,
            actions: Vec::new(),
        };

        assert!(!checker.check(&config).await);
    }
}
