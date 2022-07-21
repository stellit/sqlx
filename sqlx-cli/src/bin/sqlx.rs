use clap::Parser;
use console::style;
use dotenvy::dotenv;
use sqlx_cli::Opt;

#[tokio::main]
async fn main() {
    dotenv().ok();
    // no special handling here
    if let Err(error) = sqlx_cli::run(Opt::parse()).await {
        println!("{} {}", style("error:").bold().red(), error);
        std::process::exit(1);
    }
}
