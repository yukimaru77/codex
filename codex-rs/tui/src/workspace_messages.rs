use crate::legacy_core::config::Config;
use codex_backend_client::Client as BackendClient;
use codex_backend_client::CodexWorkspaceMessage;
use codex_login::AuthManager;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::timeout;

const HEADLINE_FETCH_TIMEOUT: Duration = Duration::from_millis(1000);

static WORKSPACE_HEADLINE: OnceLock<Option<CodexWorkspaceMessage>> = OnceLock::new();

pub(crate) fn prewarm_headline(config: &Config) {
    if WORKSPACE_HEADLINE.get().is_some() {
        return;
    }

    let config = config.clone();
    tokio::spawn(async move {
        let headline = timeout(HEADLINE_FETCH_TIMEOUT, fetch_headline(config))
            .await
            .ok()
            .flatten();
        let _ = WORKSPACE_HEADLINE.set(headline);
    });
}

async fn fetch_headline(config: Config) -> Option<CodexWorkspaceMessage> {
    let auth_manager =
        AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false).await;
    let auth = auth_manager.auth().await?;
    if !auth.uses_codex_backend() {
        return None;
    }

    let client = BackendClient::from_auth(config.chatgpt_base_url, &auth).ok()?;
    let messages = client.list_workspace_messages().await.ok()?;
    messages.headlines().next().cloned()
}
