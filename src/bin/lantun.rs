use clap::Parser;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let args = lantun::cli::Args::parse();
    lantun::cli::run(args).await
}
