use std::{
    collections::{hash_map::Entry, HashMap},
    fmt::Display,
};

use anyhow::Context;
use futures::StreamExt;
use k8s_openapi::api::core::v1::{ContainerStatus, Pod};
use kube::{
    api::{Api, LogParams, ResourceExt},
    runtime::watcher,
    Client,
};
use tokio::sync::mpsc;
use wildmatch::WildMatch;

use crate::message;

/// Key: container name
/// Value: container restart count
type RestartCounts = HashMap<String, i32>;

/// Number of log lines to fetch
const LOG_LINES: i64 = 500;

/// After container restarted more than `NOTIFICATION_SKIP_THRESHOLD` times,
/// notifications will be sent every `NOTIFICATION_SKIP_INTERVAL` restarts.
/// e.g. 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 34, 58, 82, ... th
/// restarts will be notified.
const NOTIFICATION_SKIP_THRESHOLD: i32 = 10;

/// See `NOTIFICATION_SKIP_THRESHOLD` for details.
/// Container restart back-off is 5 minutes, so notification interval
/// is approximately 2 hours.
const NOTIFICATION_SKIP_INTERVAL: i32 = 24;

/// Task to watch events in kubernetes cluster
pub async fn watch(
    client: Client,
    tx: mpsc::Sender<message::ContainerRestartInfo>,
) -> anyhow::Result<()> {
    // Read pods in all namespaces into the typed interface from k8s-openapi
    let pods: Api<Pod> = Api::all(client.clone());

    let notification_config =
        std::env::var("SLACK_NOTIFICATION_CONFIG")?.parse::<NotificationConfig>()?;

    // Map Pod UID -> container name -> container restart count
    let mut pod_restart_count = HashMap::<String, RestartCounts>::new();

    let mut event_stream = watcher(pods, watcher::Config::default()).boxed();
    while let Some(res) = event_stream.next().await {
        let e = match res {
            Ok(e) => e,
            Err(err) => {
                log::error!("Failure in watcher: {err}");
                continue;
            }
        };
        match e {
            // Pod `p` was added or modified.
            // Note that a container restart is treated as a modification of pod status.
            watcher::Event::Apply(p) => {
                process_applied(
                    &mut pod_restart_count,
                    &notification_config,
                    &client,
                    &p,
                    &tx,
                )
                .await?;
            }
            // Pod `p` was terminated successfully.
            watcher::Event::Delete(p) => {
                log::info!("Pod deleted: {}", PodDisplay(&p));
                pod_restart_count.remove(&p.uid().unwrap());
            }
            // `watcher` was initialized or restarted.
            watcher::Event::Init => {
                pod_restart_count.clear();
            }
            // Register all living pods in `pod_restart_count`.
            watcher::Event::InitApply(p) => {
                log::info!("Pod detected: {}", PodDisplay(&p));
                pod_restart_count.insert(p.uid().unwrap(), restarts_in_pod(&p));
            }
            watcher::Event::InitDone => (),
        }
    }

    Ok(())
}

/// Processes `watcher::Event::Applied` event
async fn process_applied(
    pod_restart_count: &mut HashMap<String, RestartCounts>,
    notification_config: &NotificationConfig,
    client: &Client,
    p: &Pod,
    tx: &mpsc::Sender<message::ContainerRestartInfo>,
) -> anyhow::Result<()> {
    match pod_restart_count.entry(p.uid().unwrap()) {
        Entry::Occupied(mut entry) => {
            for container in containers(p) {
                let current_restart = entry.get_mut().entry(container.name.clone()).or_default();
                if container.restart_count <= *current_restart {
                    continue;
                }
                *current_restart = container.restart_count;
                if is_skipped_interval(container.restart_count) {
                    continue;
                }
                log::info!(
                    "Container restarted: {} - {}",
                    PodDisplay(p),
                    &container.name
                );
                let channel = match notification_config.find_channel(
                    p.namespace().as_deref().unwrap_or(""),
                    &p.name_any(),
                    &container.name,
                ) {
                    // Notify to specified channel
                    Some(channel) => channel,
                    // Skip notification
                    None => {
                        log::debug!(
                            "Skipping notification: {} - {}",
                            PodDisplay(p),
                            &container.name
                        );
                        continue;
                    }
                };
                let message =
                    describe_container_status(client.clone(), p, container, channel).await;
                log::debug!(
                    "Message queue capacity: {} / {}",
                    tx.capacity(),
                    tx.max_capacity()
                );
                tx.send(message).await?;
            }
        }
        // Pod `p` did not exist until this event
        Entry::Vacant(entry) => {
            log::info!("New pod created: {}", PodDisplay(p));
            entry.insert(restarts_in_pod(p));
        }
    }

    Ok(())
}

fn is_skipped_interval(restart_count: i32) -> bool {
    restart_count > NOTIFICATION_SKIP_THRESHOLD
        && (restart_count - NOTIFICATION_SKIP_THRESHOLD) % NOTIFICATION_SKIP_INTERVAL != 0
}

/// Collect restart count of containers in Pod `p`.
fn restarts_in_pod(p: &Pod) -> RestartCounts {
    containers(p)
        .map(|st| (st.name.clone(), st.restart_count))
        .collect()
}

fn containers(p: &Pod) -> impl Iterator<Item = &ContainerStatus> {
    p.status
        .iter()
        .flat_map(|st| st.container_statuses.iter().flatten())
}

/// Helper struct to display Pod by namespace and name
struct PodDisplay<'a>(&'a Pod);

impl Display for PodDisplay<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.0.namespace().unwrap(), self.0.name_any(),)
    }
}

/// Describes status and logs of Container `container` in Pod `p`.
async fn describe_container_status(
    client: Client,
    p: &Pod,
    container: &ContainerStatus,
    channel: &str,
) -> message::ContainerRestartInfo {
    let pods_ns: Api<Pod> = Api::namespaced(client, p.namespace().as_ref().unwrap());
    let logs = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        pods_ns.logs(
            &p.name_any(),
            &LogParams {
                container: Some(container.name.clone()),
                previous: true,
                tail_lines: Some(LOG_LINES),
                ..Default::default()
            },
        ),
    )
    .await;
    log::debug!("Fetched container logs: {logs:?}");
    let logs = logs
        .map_err(|_| "timeout elapsed".to_owned())
        .and_then(|res| res.map_err(|err| err.to_string()));
    message::ContainerRestartInfo {
        namespace: p.namespace(),
        pod_name: p.name_any(),
        container_name: container.name.clone(),
        container_image: container.image.clone(),
        node_name: p.spec.as_ref().and_then(|s| s.node_name.clone()),
        restart_count: container.restart_count,
        last_state: get_last_state(container),
        resources: get_resources(p, container).unwrap_or_default(),
        logs: message::ContainerLog(logs),
        channel: channel.to_owned(),
    }
}

fn get_last_state(container: &ContainerStatus) -> Option<message::ContainerState> {
    let state = container.last_state.as_ref()?.terminated.as_ref()?;
    Some(message::ContainerState {
        exit_code: state.exit_code,
        signal: state.signal,
        reason: state.reason.clone(),
        message: state.message.clone(),
        started_at: state.started_at.as_ref().map(|t| t.0.to_rfc3339()),
        finished_at: state.finished_at.as_ref().map(|t| t.0.to_rfc3339()),
    })
}

fn get_resources(p: &Pod, container: &ContainerStatus) -> Option<message::ContainerResources> {
    let resources = p
        .spec
        .as_ref()?
        .containers
        .iter()
        .find(|&c| c.name == container.name)?
        .resources
        .as_ref()?;
    Some(message::ContainerResources {
        limits: resources.limits.as_ref().map_or_else(Vec::new, |m| {
            m.iter().map(|(k, v)| (k.clone(), (v.0.clone()))).collect()
        }),
        requests: resources.requests.as_ref().map_or_else(Vec::new, |m| {
            m.iter().map(|(k, v)| (k.clone(), (v.0.clone()))).collect()
        }),
    })
}

/// Rule to control notification destination.
/// `namespace/pod/container=channel` format.
/// namespace, pod and container name can include `*` wildcard.
/// Notification is disabled when channel is empty.
#[derive(Debug, Clone, PartialEq)]
struct NotificationRule {
    namespace: WildMatch,
    pod: WildMatch,
    container: WildMatch,
    /// `None` disables notification
    channel: Option<String>,
}

impl std::str::FromStr for NotificationRule {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (namespace, pod, container, channel) = (|| {
            let (namespace, rest) = s.split_once('/')?;
            let (pod, rest) = rest.split_once('/')?;
            let (container, channel) = rest.split_once('=')?;
            Some((namespace, pod, container, channel))
        })()
        .with_context(|| format!("Invalid notification rule: {}", s))?;
        let channel = if channel.is_empty() {
            None
        } else {
            Some(channel.to_owned())
        };
        Ok(Self {
            namespace: WildMatch::new(namespace),
            pod: WildMatch::new(pod),
            container: WildMatch::new(container),
            channel,
        })
    }
}

impl NotificationRule {
    fn find_channel(&self, namespace: &str, pod: &str, container: &str) -> Option<Option<&str>> {
        (self.namespace.matches(namespace)
            && self.pod.matches(pod)
            && self.container.matches(container))
        .then_some(self.channel.as_deref())
    }
}

/// Set of `NotificationRule`s to control notification destination.
/// `namespace/pod/container=channel,namespace/pod/container=channel,...` format.
/// Earlier rules have higher priority.
#[derive(Debug, Default, Clone, PartialEq)]
struct NotificationConfig(Vec<NotificationRule>);

impl std::str::FromStr for NotificationConfig {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.split(',')
            .map(|rule| rule.parse())
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }
}

impl NotificationConfig {
    fn find_channel(&self, namespace: &str, pod: &str, container: &str) -> Option<&str> {
        self.0
            .iter()
            .find_map(|rule| rule.find_channel(namespace, pod, container))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_skipped_interval() {
        for count in 1..11 {
            assert!(!is_skipped_interval(count));
        }
        for count in 11..34 {
            assert!(is_skipped_interval(count));
        }
        assert!(!is_skipped_interval(34));
    }

    #[test]
    fn test_notification_rule_parse() {
        assert_eq!(
            "foo/bar/baz=qux".parse::<NotificationRule>().unwrap(),
            NotificationRule {
                namespace: WildMatch::new("foo"),
                pod: WildMatch::new("bar"),
                container: WildMatch::new("baz"),
                channel: Some("qux".to_owned()),
            }
        );
        assert_eq!(
            "foo/bar/baz=".parse::<NotificationRule>().unwrap(),
            NotificationRule {
                namespace: WildMatch::new("foo"),
                pod: WildMatch::new("bar"),
                container: WildMatch::new("baz"),
                channel: None,
            }
        );
    }

    #[test]
    fn test_notification_config_parse() {
        assert_eq!(
            "foo/bar/baz=qux".parse::<NotificationConfig>().unwrap(),
            NotificationConfig(vec![NotificationRule {
                namespace: WildMatch::new("foo"),
                pod: WildMatch::new("bar"),
                container: WildMatch::new("baz"),
                channel: Some("qux".to_owned()),
            }])
        );
        assert_eq!(
            "foo/bar/baz=qux,*/*/*=default"
                .parse::<NotificationConfig>()
                .unwrap(),
            NotificationConfig(vec![
                NotificationRule {
                    namespace: WildMatch::new("foo"),
                    pod: WildMatch::new("bar"),
                    container: WildMatch::new("baz"),
                    channel: Some("qux".to_owned()),
                },
                NotificationRule {
                    namespace: WildMatch::new("*"),
                    pod: WildMatch::new("*"),
                    container: WildMatch::new("*"),
                    channel: Some("default".to_owned()),
                }
            ])
        );
    }

    #[test]
    fn test_notification_config_find_channel() {
        let config = "foo/bar/baz=qux,ignore/*/*=,foo/*/*=default"
            .parse::<NotificationConfig>()
            .unwrap();
        assert_eq!(config.find_channel("foo", "bar", "baz"), Some("qux"));
        assert_eq!(config.find_channel("foo", "bar", "qux"), Some("default"));
        assert_eq!(config.find_channel("ignore", "bar", "baz"), None);
        assert_eq!(config.find_channel("nomatch", "bar", "baz"), None);
    }
}
