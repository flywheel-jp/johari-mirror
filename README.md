# johari-mirror

[![Rust build and test](https://github.com/flywheel-jp/johari-mirror/actions/workflows/rust.yml/badge.svg)](https://github.com/flywheel-jp/johari-mirror/actions/workflows/rust.yml)

johari-mirror monitors a Kubernetes cluster to detect container restarts and
notify restart reasons and logs to Slack.

## Overview

TODO

## Installation

You can use [example.yaml](deployment/example.yaml) to deploy johari-mirror to your
Kubernetes cluster with `NAMESPACE` and `NOTIFICATION_CHANNEL` replaced.

```sh
kubectl create secret generic johari-mirror-slack-api-token \
  --from-literal=token=<your-slack-token>
kubectl apply -f example.yaml
```

### Environment variables

All environment variables are required.

| Name | Description |
|:--|:--|
| `SLACK_TOKEN` | Slack Bot User OAuth Token. See Slack authentication section. |
| `SLACK_NOTIFICATION_CONFIG` | Filters to configure notification destination. See the following section. |

#### SLACK_NOTIFICATION_CONFIG

### Slack authentication

[Quickstart | Slack](https://api.slack.com/start/quickstart)

Create a Slack App and install it to your workspace.
johari-mirror uses
[`Bot User OAuth Token`](https://api.slack.com/authentication/token-types#bot)
in the environment variable `SLACK_TOKEN`.

#### Required permission scopes

- Bot Token Scopes
  - `chat:write.public` or `chat:write`
    - With `chat:write`, the app needs to be invited to the target Slack channels.
  - `files:write`

### Kubernetes authentication

Kubernetes authentication can be obtained from `KUBECONFIG`, `~/.kube/config` or
in-cluster config.
cf. [Config in kube - Rust](https://docs.rs/kube/latest/kube/struct.Config.html#method.infer)

See example manifest.

#### Required permissions

- Resources: `pods`, `pods/log`
- Verbs: `get`, `watch`, `list`

## License

MIT

## Related projects

- [airwallex/k8s-pod-restart-info-collector](https://github.com/airwallex/k8s-pod-restart-info-collector)
