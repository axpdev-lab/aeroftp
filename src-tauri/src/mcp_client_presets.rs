//! Curated presets. The renderer selects an id and never supplies a URL, hash,
//! command or manifest body.
// SPDX-License-Identifier: GPL-3.0-or-later
use serde::Serialize;
use tauri::{AppHandle, Webview};

pub const DEEPWIKI_ENDPOINT: &str = "https://mcp.deepwiki.com/mcp";

/// A reviewed HTTP preset card. STDIO presets come from the reviewed manifest
/// registry once they ship with a pinned download (MCLIENT-07 v2).
#[derive(Serialize)]
pub struct PresetView {
    pub id: &'static str,
    pub transport: &'static str,
    pub publisher: &'static str,
    pub homepage: &'static str,
    pub endpoint: &'static str,
}

const HTTP_PRESETS: &[PresetView] = &[PresetView {
    id: "deepwiki",
    transport: "http",
    publisher: "DeepWiki",
    homepage: "https://deepwiki.com",
    endpoint: DEEPWIKI_ENDPOINT,
}];

#[tauri::command]
pub async fn mcp_client_presets_list(
    webview: Webview,
) -> Result<&'static [PresetView], &'static str> {
    crate::only_main_window(webview.label(), "mcp_client_presets_list")
        .map_err(|_| "MCP_MAIN_WINDOW_REQUIRED")?;
    Ok(HTTP_PRESETS)
}

#[tauri::command]
pub async fn mcp_client_preset_install_http(
    webview: Webview,
    app: AppHandle,
    preset_id: String,
) -> Result<(), &'static str> {
    crate::only_main_window(webview.label(), "mcp_client_preset_install_http")
        .map_err(|_| "MCP_MAIN_WINDOW_REQUIRED")?;
    let preset = HTTP_PRESETS
        .iter()
        .find(|preset| preset.id == preset_id)
        .ok_or("MCP_PRESET_UNKNOWN")?;
    // The reviewed constant must still parse as a public HTTPS endpoint, unchanged.
    let endpoint = crate::mcp_client_http_config::parse_public_https(preset.endpoint, 2048)
        .map_err(|_| "MCP_HTTP_INVALID_ENDPOINT")?;
    if endpoint.as_str().trim_end_matches('/') != preset.endpoint.trim_end_matches('/') {
        return Err("MCP_HTTP_INVALID_ENDPOINT");
    }
    let (id, endpoint) = (preset.id, preset.endpoint);
    tokio::task::spawn_blocking(move || {
        crate::mcp_client_http_commands::install_disabled_endpoint(&app, id, endpoint)
    })
    .await
    .map_err(|_| "MCP_STORE_UNAVAILABLE")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepwiki_is_the_only_preset_and_its_endpoint_is_the_approved_one() {
        let endpoint =
            crate::mcp_client_http_config::parse_public_https(DEEPWIKI_ENDPOINT, 2048).unwrap();
        assert_eq!(endpoint.path(), "/mcp");
        assert_eq!(endpoint.host_str(), Some("mcp.deepwiki.com"));
        let ids = HTTP_PRESETS.iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(ids, ["deepwiki"]);
        assert!(HTTP_PRESETS
            .iter()
            .all(|p| p.transport == "http" && p.endpoint == DEEPWIKI_ENDPOINT));
    }

    #[tokio::test]
    async fn deepwiki_live_accepts_two_tools_when_opted_in() {
        if std::env::var("AEROFTP_MCP_PRESET_LIVE").as_deref() != Ok("1") {
            return;
        }
        use crate::mcp_client_http_config::{McpHttpAuth, McpHttpServerConfig};
        use crate::mcp_client_http_transport::{HttpBinding, HttpError, HttpSession};
        let endpoint = DEEPWIKI_ENDPOINT.to_string();
        let config = McpHttpServerConfig {
            id: "deepwiki".into(),
            endpoint: endpoint.clone(),
            auth: McpHttpAuth::None,
            enabled: true,
            revision: 1,
        };
        let binding = HttpBinding {
            endpoint,
            user_id: 1,
            revision: "1".into(),
        };
        let mut session = HttpSession::new(binding.clone());
        let mut fresh = || Ok::<(), HttpError>(());
        let result = session
            .call_checked(
                &config,
                &binding,
                "tools/list",
                serde_json::Map::new(),
                None,
                None,
                &tokio_util::sync::CancellationToken::new(),
                &mut fresh,
            )
            .await
            .expect("DeepWiki tools/list");
        let names = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        let accepted = names
            .iter()
            .filter(|name| crate::mcp_client_schema::discover(&result, name).is_ok())
            .copied()
            .collect::<Vec<_>>();
        assert!(
            crate::mcp_client_schema::discover(&result, "ask_wiki_question").is_err(),
            "{names:?}"
        );
        assert_eq!(accepted.len(), 2, "{names:?}");
        assert!(!accepted.iter().any(|name| name.contains("aeroftp")));
    }
}
