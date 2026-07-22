use anyhow::Result;
use serde::Deserialize;

use super::{ProviderStatus, StatusLevel, http_client};

#[derive(Debug, Clone)]
pub struct StatusTarget {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub page_url: &'static str,
}

const TARGETS: &[StatusTarget] = &[
    StatusTarget {
        id: "codex",
        name: "OpenAI",
        url: "https://status.openai.com/api/v2/status.json",
        page_url: "https://status.openai.com/",
    },
    StatusTarget {
        id: "claude",
        name: "Anthropic",
        url: "https://status.anthropic.com/api/v2/status.json",
        page_url: "https://status.anthropic.com/",
    },
    StatusTarget {
        id: "copilot",
        name: "GitHub",
        url: "https://www.githubstatus.com/api/v2/status.json",
        page_url: "https://www.githubstatus.com/",
    },
];

#[derive(Debug, Deserialize)]
struct StatusPageResponse {
    status: StatusPageStatus,
}

#[derive(Debug, Deserialize)]
struct StatusPageStatus {
    indicator: String,
    description: String,
}

pub async fn fetch_all() -> Vec<(StatusTarget, ProviderStatus)> {
    let mut out = Vec::new();
    for target in TARGETS {
        let status = match fetch_one(target).await {
            Ok(s) => s,
            Err(e) => ProviderStatus {
                level: StatusLevel::Unknown,
                message: format!("status unavailable: {}", e),
                url: Some(target.page_url.to_string()),
            },
        };
        out.push((target.clone(), status));
    }
    out
}

pub async fn fetch_for_provider(id: &str) -> Option<ProviderStatus> {
    let target = TARGETS.iter().find(|target| target.id == id)?;
    Some(match fetch_one(target).await {
        Ok(s) => s,
        Err(e) => ProviderStatus {
            level: StatusLevel::Unknown,
            message: format!("status unavailable: {}", e),
            url: Some(target.page_url.to_string()),
        },
    })
}

async fn fetch_one(target: &StatusTarget) -> Result<ProviderStatus> {
    let resp: StatusPageResponse = http_client()?
        .get(target.url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(ProviderStatus {
        level: map_indicator(&resp.status.indicator),
        message: resp.status.description,
        url: Some(target.page_url.to_string()),
    })
}

fn map_indicator(indicator: &str) -> StatusLevel {
    match indicator {
        "none" => StatusLevel::Operational,
        "minor" | "maintenance" => StatusLevel::Degraded,
        "major" | "critical" => StatusLevel::Outage,
        _ => StatusLevel::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_statuspage_indicators() {
        assert!(matches!(map_indicator("none"), StatusLevel::Operational));
        assert!(matches!(map_indicator("minor"), StatusLevel::Degraded));
        assert!(matches!(map_indicator("major"), StatusLevel::Outage));
        assert!(matches!(map_indicator("critical"), StatusLevel::Outage));
        assert!(matches!(map_indicator("weird"), StatusLevel::Unknown));
    }
}
