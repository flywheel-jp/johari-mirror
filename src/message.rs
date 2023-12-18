use serde_json::json;

/// Number of log lines to include in the main message
const LOG_SUMMARY_LINES: usize = 20;

/// Maximum length of `text` in `section` blocks in Slack messages
/// https://api.slack.com/reference/block-kit/blocks#section_fields
const SECTION_TEXT_LIMIT: usize = 3000;

/// Number of characters to include in the log summary.
/// Set 200 characters margin for header and footer.
const LOG_SUMMARY_CHARS: usize = SECTION_TEXT_LIMIT - 200;

#[derive(Debug)]
pub struct ContainerRestartInfo {
    pub namespace: Option<String>,
    pub pod_name: String,
    pub container_name: String,
    pub container_image: String,
    pub restart_count: i32,
    pub last_state: Option<ContainerState>,
    pub resources: ContainerResources,
    pub logs: ContainerLog,
    pub channel: String,
}

impl ContainerRestartInfo {
    pub fn to_message(&self, file_url: &Option<String>) -> serde_json::Value {
        let container_identity = format!(
            r"Namespace: {}
Pod: `{}`
Container Name: `{}`
Container Image: `{}`",
            format_name(&self.namespace),
            &self.pod_name,
            &self.container_name,
            &self.container_image,
        );
        let stats = build_container_stats(self.restart_count, &self.last_state);
        let resources = self.resources.to_message();
        let logs = self.logs.to_message(file_url);

        json!([
            {
                "type": "header",
                "text": {
                    "type": "plain_text",
                    "text": "Container restarted",
                },
            },
            {
                "type": "section",
                "text": markdown_text(&container_identity),
            },
            {
                "type": "section",
                "fields": stats,
            },
            {
                "type": "section",
                "fields": resources,
            },
            {
                "type": "section",
                "text": markdown_text(&logs),
            },
        ])
    }
}

impl std::fmt::Display for ContainerRestartInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{} - {}",
            self.namespace.as_deref().unwrap_or(""),
            self.pod_name,
            self.container_name,
        )
    }
}

fn build_container_stats(
    restart_count: i32,
    state: &Option<ContainerState>,
) -> Vec<serde_json::Value> {
    let mut container_stats = vec![markdown_text(&format!(
        "Restart Count: `{}`",
        restart_count
    ))];
    if let Some(state) = state {
        container_stats.push(markdown_text(" ")); // alignment
        container_stats.push(markdown_text(&format!("Exit Code: `{}`", state.exit_code)));
        let signal = state
            .signal
            .map_or_else(|| "none".to_owned(), |s| format!("`{}`", s));
        container_stats.push(markdown_text(&format!("Signal: {}", signal)));
        container_stats.push(markdown_text(&format!(
            "Reason: {}",
            format_name(&state.reason)
        )));
        container_stats.push(markdown_text(&format!(
            "Message: {}",
            format_name(&state.message)
        )));
        container_stats.push(markdown_text(&format!(
            "Started at: {}",
            format_name(&state.started_at)
        )));
        container_stats.push(markdown_text(&format!(
            "Finished at: {}",
            format_name(&state.finished_at)
        )));
    }
    container_stats
}

#[derive(Debug)]
pub struct ContainerState {
    pub exit_code: i32,
    pub signal: Option<i32>,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Default)]
pub struct ContainerResources {
    pub limits: Vec<(String, String)>,
    pub requests: Vec<(String, String)>,
}

impl ContainerResources {
    fn to_message(&self) -> Vec<serde_json::Value> {
        if self.limits.is_empty() && self.requests.is_empty() {
            return vec![markdown_text("No resource limits or requests")];
        }
        let mut message = Vec::new();
        for (resource, quantity) in &self.limits {
            message.push(markdown_text(&format!("{resource} limit: `{quantity}`")));
        }
        for (resource, quantity) in &self.requests {
            message.push(markdown_text(&format!("{resource} request: `{quantity}`")));
        }
        message
    }
}

#[derive(Debug)]
pub struct ContainerLog(pub Result<String, String>);

impl ContainerLog {
    fn to_message(&self, file_url: &Option<String>) -> String {
        match &self.0 {
            Ok(log) => {
                if log.is_empty() {
                    "*Container logs before restart*\n(empty)".to_owned()
                } else {
                    // file_url is non-empty when log is not empty
                    let file_url = file_url.as_deref().unwrap_or_default();
                    format!(
                        r"<{}|*Container logs before restart*>
```
{}
```",
                        file_url,
                        Self::tail_lines(log)
                    )
                }
            }
            Err(err) => format!("Failed to get container logs: {}", err),
        }
    }

    /// Returns suffix of `log`, shorter one of:
    /// - last `LOG_SUMMARY_LINES` lines
    /// - last `LOG_SUMMARY_CHARS` characters
    fn tail_lines(log: &str) -> String {
        let mut lines = log
            .lines()
            .rev()
            .take(LOG_SUMMARY_LINES)
            .collect::<Vec<_>>();
        lines.reverse();
        suffix(&lines.join("\n"), LOG_SUMMARY_CHARS).to_owned()
    }
}

fn format_name(name: &Option<impl AsRef<str>>) -> String {
    if let Some(name) = name.as_ref() {
        format!("`{}`", name.as_ref())
    } else {
        "unknown".to_owned()
    }
}

fn markdown_text(text: &str) -> serde_json::Value {
    json!({
        "type": "mrkdwn",
        "text": text,
    })
}

/// Returns the last `limit` characters of `text`
fn suffix(text: &str, limit: usize) -> &str {
    if limit == 0 {
        return "";
    }
    // string slicing is in bytes, not chars, so we need to count chars
    let char_count = text.chars().count();
    if let Some(begin_char) = char_count.checked_sub(limit) {
        let begin_char = begin_char.min(char_count);
        let begin_byte = text.char_indices().nth(begin_char).unwrap().0;
        &text[begin_byte..]
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suffix() {
        assert_eq!(suffix("hello", 6), "hello");
        assert_eq!(suffix("hello", 5), "hello");
        assert_eq!(suffix("hello", 4), "ello");
        assert_eq!(suffix("hello", 3), "llo");
        assert_eq!(suffix("hello", 2), "lo");
        assert_eq!(suffix("hello", 1), "o");
        assert_eq!(suffix("hello", 0), "");
    }

    #[test]
    fn test_suffix_multibyte() {
        assert_eq!(suffix("こんにちは", 6), "こんにちは");
        assert_eq!(suffix("こんにちは", 5), "こんにちは");
        assert_eq!(suffix("こんにちは", 4), "んにちは");
        assert_eq!(suffix("こんにちは", 3), "にちは");
        assert_eq!(suffix("こんにちは", 2), "ちは");
        assert_eq!(suffix("こんにちは", 1), "は");
        assert_eq!(suffix("こんにちは", 0), "");
    }
}
