use chrono_tz::Europe::Madrid;
use dbot::{
    Bot,
    BotConfig,
    SleepSchedule,
};
use serenity::model::id::{
    GuildId,
    UserId,
};

#[tokio::main]
async fn main() {
    let discord_token =
        std::env::var("DISCORD_TOKEN")
            .expect("DISCORD_TOKEN is required");

    let nvidia_api_key =
        std::env::var("NVIDIA_API_KEY")
            .expect("NVIDIA_API_KEY is required");

    let sleep = SleepSchedule::daily(
        GuildId::new(1206341039548403764),
        UserId::new(893892818068725780),
        24,
        00,
        Madrid,
    )
    .expect("invalid sleep schedule")
    .reason("Dad says it is bedtime.");

    let config = BotConfig::new(
        discord_token,
        nvidia_api_key,
    )
    .memory_size(30)
    .sleep_schedule(sleep);

    Bot::new(config)
        .run()
        .await
        .expect("bot crashed");
}