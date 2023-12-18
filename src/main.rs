use kube::Client;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("johari-mirror=debug"),
    )
    .init();

    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    let slack_token = std::env::var("SLACK_TOKEN")?;

    let (tx, rx) = mpsc::channel(320);
    let watch_handle = tokio::spawn(johari_mirror::kubernetes::watch(client, tx));
    let slack_handle = tokio::spawn(johari_mirror::slack::slack_send(slack_token, rx));

    watch_handle.await??;
    slack_handle.await?;

    Ok(())
}
