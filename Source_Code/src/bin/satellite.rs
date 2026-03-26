#[tokio::main]
async fn main() -> anyhow::Result<()> {
    gcs::satellite::run().await
}
