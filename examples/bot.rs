use dbot::{Bot, BotConfig};

#[tokio::main]
async fn main() {
    let config = BotConfig::new(
        std::env::var("DISCORD_TOKEN")
            .expect("DISCORD_TOKEN is required"),

        std::env::var("NVIDIA_API_KEY")
            .expect("NVIDIA_API_KEY is required"),
    );

    Bot::new(config)
        .run()
        .await
        .expect("bot crashed");
}