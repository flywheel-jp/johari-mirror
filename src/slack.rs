use anyhow::{bail, Context};
use tokio::sync::mpsc;

use crate::message;

const POST_MESSAGE_URL: &str = "https://slack.com/api/chat.postMessage";
const UPLOAD_FILE_URL: &str = "https://slack.com/api/files.upload";

/// Task to send messages to Slack channel
pub async fn slack_send(
    slack_token: String,
    mut rx: mpsc::Receiver<message::ContainerRestartInfo>,
) {
    let slack = reqwest::Client::new();

    while let Some(restart_info) = rx.recv().await {
        log::debug!("Start sending message to Slack: {restart_info}");
        if let Err(e) = post_notification(&slack, &slack_token, &restart_info).await {
            log::error!("Failed to post message to Slack: {e}");
        }
        log::debug!("Finished sending message to Slack: {restart_info}");
    }
}

async fn post_notification(
    slack: &reqwest::Client,
    slack_token: &str,
    restart_info: &message::ContainerRestartInfo,
) -> anyhow::Result<()> {
    let file_url = upload_log_file(slack, slack_token, restart_info).await?;
    post_message(
        slack,
        slack_token,
        &restart_info.channel,
        restart_info,
        &file_url,
    )
    .await
}

async fn upload_log_file(
    slack: &reqwest::Client,
    slack_token: &str,
    restart_info: &message::ContainerRestartInfo,
) -> anyhow::Result<Option<String>> {
    let log = match restart_info.logs.0.as_ref().map(|log| log.trim_end()) {
        Ok(log) if !log.is_empty() => log,
        _empty_or_error => return Ok(None),
    };
    let title = format!(
        "{}_{}_{}",
        restart_info.namespace.as_ref().unwrap_or(&"".to_owned()),
        &restart_info.pod_name,
        &restart_info.container_name
    );

    let params = [
        ("content", log),
        ("filename", &title),
        ("filetype", "text"),
        ("title", &title),
    ];
    let resp = slack
        .post(UPLOAD_FILE_URL)
        .bearer_auth(slack_token)
        .form(&params)
        .send()
        .await?;
    let resp = parse_slack_response(resp).await?;
    let file_url =
        get_file_url_from_response(&resp).context("Failed to get file URL from response")?;
    Ok(Some(file_url.to_owned()))
}

async fn post_message(
    slack: &reqwest::Client,
    slack_token: &str,
    slack_channel: &str,
    restart_info: &message::ContainerRestartInfo,
    file_url: &Option<String>,
) -> anyhow::Result<()> {
    let message = serde_json::json!({
        "channel": slack_channel,
        "blocks": restart_info.to_message(file_url),
        "unfurl_links": false,
    });
    let resp = slack
        .post(POST_MESSAGE_URL)
        .bearer_auth(slack_token)
        .json(&message)
        .send()
        .await?;
    parse_slack_response(resp).await?;
    Ok(())
}

async fn parse_slack_response(resp: reqwest::Response) -> anyhow::Result<serde_json::Value> {
    if !resp.status().is_success() {
        bail!(
            "Failed to post message to Slack: {}",
            resp.text().await.unwrap_or_else(|err| err.to_string())
        );
    }
    log::debug!("Response from Slack: status={}", resp.status());
    let resp: serde_json::Value = resp.json().await?;
    if !matches!(resp.get("ok"), Some(serde_json::Value::Bool(true))) {
        if let Some(error) = resp.get("error") {
            bail!("Slack response is not ok: {}", error);
        } else {
            bail!("Unexpected Slack response format: {:?}", resp);
        }
    }
    Ok(resp)
}

fn get_file_url_from_response(resp: &serde_json::Value) -> Option<&str> {
    resp.get("file")?.get("permalink")?.as_str()
}
