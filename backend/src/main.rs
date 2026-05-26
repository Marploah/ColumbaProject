#[tokio::main]
async fn main() -> anyhow::Result<()> {
    columba_backend::run().await
}
