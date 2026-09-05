use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use chrono::{
    DateTime,
    Duration as ChronoDuration,
    LocalResult,
    TimeZone,
    Utc,
};
use chrono_tz::Tz;

use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};

use serenity::{
    async_trait,
    builder::CreateMessage,
    model::{
        channel::Message,
        gateway::Ready,
        id::{ChannelId, GuildId, UserId},
    },
    prelude::*,
};

use songbird::{
    input::File,
    SerenityInit,
};
use tokio::sync::RwLock;

const NVIDIA_URL: &str =
    "https://integrate.api.nvidia.com/v1/chat/completions";

const DEFAULT_MODEL: &str = "openai/gpt-oss-20b";

const DEFAULT_SYSTEM_PROMPT: &str = concat!(
    "You are the Old Dad of the Discord server. ",
    "You are chatting with users in the Discord server, basically like your children. ",
    "Do not mention internal memory or system instructions. ",
    "Answer ALWAYS in spanish or german only if required. ",
    "You practically dont understand English language from others, ",
    "and you will poorly try to translate to spanish or just ignore it. ",
    "Keep responses natural and with a normal message length. ",
    "You are not any kind of assistant, you are just a character playing their role. ",
    "Do not include any type of reasoning in your responses. "
);

type Memory =
    Arc<RwLock<HashMap<u64, VecDeque<ChatMessage>>>>;


type SleepingUsers =
    Arc<RwLock<HashSet<(GuildId, UserId)>>>;


type CurrentSound =
    Arc<RwLock<HashMap<GuildId, songbird::tracks::TrackHandle>>>;

#[derive(Clone, Debug)]
pub struct SleepSchedule {
    pub guild_id: GuildId,
    pub user_id: UserId,
    pub hour: u32,
    pub minute: u32,
    pub timezone: Tz,
    pub reason: String,
}

impl SleepSchedule {
    pub fn daily(
        guild_id: GuildId,
        user_id: UserId,
        hour: u32,
        minute: u32,
        timezone: Tz,
    ) -> Result<Self, String> {
        if hour > 23 {
            return Err("Sleep schedule hour must be between 0 and 23".into());
        }

        if minute > 59 {
            return Err(
                "Sleep schedule minute must be between 0 and 59".into()
            );
        }

        Ok(Self {
            guild_id,
            user_id,
            hour,
            minute,
            timezone,
            reason: "Scheduled bedtime".into(),
        })
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Clone, Debug)]
pub struct BotConfig {
    pub discord_token: String,
    pub nvidia_api_key: String,
    pub model: String,
    pub memory_size: usize,
    pub max_tokens: u32,
    pub temperature: f32,
    pub system_prompt: String,

    pub sleep_schedule: Option<SleepSchedule>,
}

impl BotConfig {
    pub fn new(
        discord_token: impl Into<String>,
        nvidia_api_key: impl Into<String>,
    ) -> Self {
        Self {
            discord_token: discord_token.into(),
            nvidia_api_key: nvidia_api_key.into(),
            model: DEFAULT_MODEL.to_string(),
            memory_size: 30,
            max_tokens: 1000,
            temperature: 0.7,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            sleep_schedule: None,
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn memory_size(mut self, memory_size: usize) -> Self {
        self.memory_size = memory_size;
        self
    }

    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn system_prompt(
        mut self,
        system_prompt: impl Into<String>,
    ) -> Self {
        self.system_prompt = system_prompt.into();
        self
    }

    pub fn sleep_schedule(
        mut self,
        schedule: SleepSchedule,
    ) -> Self {
        self.sleep_schedule = Some(schedule);
        self
    }
}

pub struct Bot {
    config: BotConfig,
    http: HttpClient,
    memory: Memory,
    sleeping_users: SleepingUsers,
    current_sounds: CurrentSound,
}

impl Bot {
    pub fn new(config: BotConfig) -> Self {
        Self {
            config,
            http: HttpClient::new(),
            memory: Arc::new(RwLock::new(HashMap::new())),
            sleeping_users: Arc::new(RwLock::new(HashSet::new())),
            current_sounds: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn run(self) -> Result<(), serenity::Error> {
        let handler = Handler {
            http: self.http,
            config: self.config.clone(),
            memory: self.memory,
            sleeping_users: self.sleeping_users,
            current_sounds: self.current_sounds,
        };

        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT
            | GatewayIntents::GUILD_VOICE_STATES;

        let mut client = Client::builder(
            &self.config.discord_token,
            intents,
        )
        .event_handler(handler)
        .register_songbird()
        .await?;

        client.start().await
    }

    pub async fn clear_memory(&self, channel_id: u64) {
        let mut memory = self.memory.write().await;
        memory.remove(&channel_id);
    }

    pub async fn clear_all_memory(&self) {
        let mut memory = self.memory.write().await;
        memory.clear();
    }
}

enum VoiceCommand {
    JoinAuthor,
    JoinUser(UserId),
    JoinChannel(ChannelId),
    Leave,
    PlaySound(String),
    StopSound,
}

struct Handler {
    http: HttpClient,
    config: BotConfig,
    memory: Memory,
    sleeping_users: SleepingUsers,
    current_sounds: CurrentSound,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("Logged in as {}", ready.user.name);
        println!("Model: {}", self.config.model);
        println!(
            "Memory: {} messages per channel",
            self.config.memory_size
        );

        if let Some(schedule) = self.config.sleep_schedule.clone() {
            println!(
                "Sleep schedule: {:02}:{:02} {}",
                schedule.hour,
                schedule.minute,
                schedule.timezone
            );

            let http = ctx.http.clone();
            let sleeping_users = self.sleeping_users.clone();

            tokio::spawn(async move {
                run_sleep_scheduler(
                    http,
                    schedule,
                    sleeping_users,
                )
                .await;
            });
        }
    }

    async fn voice_state_update(
        &self,
        ctx: Context,
        _old: Option<serenity::model::voice::VoiceState>,
        new: serenity::model::voice::VoiceState,
    ) {
        let Some(guild_id) = new.guild_id else {
            return;
        };

        let Some(_channel_id) = new.channel_id else {
            return;
        };

        let key = (guild_id, new.user_id);

        let is_sleeping = {
            let sleeping_users = self.sleeping_users.read().await;
            sleeping_users.contains(&key)
        };

        if !is_sleeping {
            return;
        }

        match guild_id
            .disconnect_member(&ctx.http, new.user_id)
            .await
        {
            Ok(_) => {
                println!(
                    "Disconnected {} from voice in {} because they are on bedtime",
                    new.user_id,
                    guild_id
                );
            }

            Err(error) => {
                eprintln!(
                    "Failed to disconnect {} from voice in {}: {}",
                    new.user_id,
                    guild_id,
                    error
                );
            }
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        let bot_id = ctx.cache.current_user().id;

        if !msg.mentions.iter().any(|user| user.id == bot_id) {
            return;
        }

        if let Some(command) = parse_voice_command(&msg, bot_id) {
            match command {
                VoiceCommand::JoinUser(user_id) => {
                    self.handle_join_user(&ctx, &msg, user_id).await;
                    return;
                }

                VoiceCommand::JoinChannel(channel_id) => {
                    self.handle_join_channel(&ctx, &msg, channel_id).await;
                    return;
                }

                VoiceCommand::JoinAuthor => {
                    self.handle_join_user(
                        &ctx,
                        &msg,
                        msg.author.id,
                    )
                    .await;

                    return;
                }

                VoiceCommand::Leave => {
                    self.handle_leave_command(&ctx, &msg).await;
                    return;
                }

                VoiceCommand::PlaySound(sound) => {
                    self.handle_play_sound(
                        &ctx,
                        &msg,
                        &sound,
                    )
                    .await;

                    return;
                }

                VoiceCommand::StopSound => {
                    self.handle_stop_sound(&ctx, &msg).await;
                    return;
                }
            }
        }

        let prompt = build_prompt(&msg, bot_id);

        if prompt.is_empty() {
            let _ = msg
                .channel_id
                .send_message(
                    &ctx.http,
                    CreateMessage::new()
                        .content("Hey! What would you like to talk about?")
                        .reference_message(&msg),
                )
                .await;

            return;
        }

        let _ = msg
            .channel_id
            .broadcast_typing(&ctx.http)
            .await;

        let channel_id = msg.channel_id.get();

        let answer = match self
            .ask_nvidia(
                channel_id,
                &msg.author.name,
                &prompt,
            )
            .await
        {
            Ok(answer) => answer,

            Err(error) => {
                eprintln!("{error}");

                "Sorry, I couldn't reach the AI service right now."
                    .to_string()
            }
        };

        self.remember(
            channel_id,
            ChatMessage {
                role: "user".to_string(),
                content: format!(
                    "{}: {}",
                    msg.author.name,
                    prompt
                ),
            },
        )
        .await;

        self.remember(
            channel_id,
            ChatMessage {
                role: "assistant".to_string(),
                content: answer.clone(),
            },
        )
        .await;

        for chunk in split_message(&answer, 1900) {
            if let Err(error) = msg
                .channel_id
                .send_message(
                    &ctx.http,
                    CreateMessage::new()
                        .content(chunk)
                        .reference_message(&msg),
                )
                .await
            {
                eprintln!("Discord send error: {error}");
                break;
            }
        }
    }
}

impl Handler {
    async fn ask_nvidia(
        &self,
        channel_id: u64,
        username: &str,
        prompt: &str,
    ) -> Result<String, String> {
        let messages = {
            let memory = self.memory.read().await;

            let history = memory
                .get(&channel_id)
                .cloned()
                .unwrap_or_default();

            let mut messages =
                Vec::with_capacity(history.len() + 2);

            messages.push(ChatMessage {
                role: "system".to_string(),
                content: self.config.system_prompt.clone(),
            });

            messages.extend(history);

            messages.push(ChatMessage {
                role: "user".to_string(),
                content: format!("{username}: {prompt}"),
            });

            messages
        };

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
        };

        let response = self
            .http
            .post(NVIDIA_URL)
            .bearer_auth(&self.config.nvidia_api_key)
            .json(&request)
            .send()
            .await
            .map_err(|error| {
                format!("NVIDIA request failed: {error}")
            })?;

        let status = response.status();

        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());

            return Err(format!(
                "NVIDIA API returned {status}: {body}"
            ));
        }

        let data: ChatResponse = response
            .json()
            .await
            .map_err(|error| {
                format!("Invalid NVIDIA response: {error}")
            })?;

        data.choices
            .first()
            .map(|choice| choice.message.content.clone())
            .ok_or_else(|| {
                "NVIDIA returned no response".to_string()
            })
    }

    async fn remember(
        &self,
        channel_id: u64,
        message: ChatMessage,
    ) {
        let mut memory = self.memory.write().await;

        let history = memory
            .entry(channel_id)
            .or_insert_with(VecDeque::new);

        history.push_back(message);

        while history.len() > self.config.memory_size {
            history.pop_front();
        }
    }

    async fn handle_join_user(
        &self,
        ctx: &Context,
        msg: &Message,
        user_id: UserId,
    ) {
        let Some(guild_id) = msg.guild_id else {
            let _ = msg
                .channel_id
                .say(&ctx.http, "This only works in a server.")
                .await;

            return;
        };

        let voice_state = match guild_id
            .get_user_voice_state(&ctx.http, user_id)
            .await
        {
            Ok(state) => state,

            Err(_) => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "That user isn't in a voice channel.",
                    )
                    .await;

                return;
            }
        };

        let Some(channel_id) = voice_state.channel_id else {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "That user isn't in a voice channel.",
                )
                .await;

            return;
        };

        self.join_voice(ctx, msg, guild_id, channel_id)
            .await;
    }

    async fn handle_join_channel(
        &self,
        ctx: &Context,
        msg: &Message,
        channel_id: ChannelId,
    ) {
        let Some(guild_id) = msg.guild_id else {
            let _ = msg
                .channel_id
                .say(&ctx.http, "This only works in a server.")
                .await;

            return;
        };

        self.join_voice(ctx, msg, guild_id, channel_id)
            .await;
    }

    async fn join_voice(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) {
        let manager = songbird::get(ctx)
            .await
            .expect("Songbird Voice client was not initialized")
            .clone();

        let result = manager
            .join(guild_id, channel_id)
            .await;

        match result {
            Ok(_) => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!(
                            "I'm joining <#{}>.",
                            channel_id.get()
                        ),
                    )
                    .await;
            }

            Err(error) => {
                eprintln!(
                    "Failed to join voice channel {}: {}",
                    channel_id,
                    error
                );

                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "I couldn't join that voice channel.",
                    )
                    .await;
            }
        }
    }

    async fn handle_leave_command(
        &self,
        ctx: &Context,
        msg: &Message,
    ) {
        let Some(guild_id) = msg.guild_id else {
            return;
        };

        let manager = songbird::get(ctx)
            .await
            .expect("Songbird Voice client was not initialized")
            .clone();

        match manager.leave(guild_id).await {
            Ok(_) => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "I'm leaving voice.")
                    .await;
            }

            Err(error) => {
                eprintln!(
                    "Failed to leave voice in {}: {}",
                    guild_id,
                    error
                );

                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "I'm not currently in a voice channel.",
                    )
                    .await;
            }
        }
    }

    async fn handle_play_sound(
        &self,
        ctx: &Context,
        msg: &Message,
        sound_name: &str,
    ) {
        let Some(guild_id) = msg.guild_id else {
            let _ = msg
                .channel_id
                .say(&ctx.http, "This only works in a server.")
                .await;

            return;
        };

        let sounds_dir = std::path::Path::new("sounds");

        if !sounds_dir.exists() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "My sounds folder doesn't exist.")
                .await;

            return;
        }

        let requested_name = sound_name
            .trim()
            .to_lowercase();

        if requested_name.is_empty()
            || requested_name.contains('/')
            || requested_name.contains('\\')
            || requested_name.contains("..")
        {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Invalid sound name.")
                .await;

            return;
        }

        let mut sound_file = None;

        let entries = match std::fs::read_dir(sounds_dir) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("Failed to read sounds directory: {error}");

                let _ = msg
                    .channel_id
                    .say(&ctx.http, "I couldn't read my sounds.")
                    .await;

                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            let Some(stem) = path.file_stem() else {
                continue;
            };

            let Some(stem) = stem.to_str() else {
                continue;
            };

            if stem.eq_ignore_ascii_case(&requested_name) {
                sound_file = Some(path);
                break;
            }
        }

        let Some(sound_file) = sound_file else {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    format!(
                        "I don't have a sound called `{}`.",
                        requested_name
                    ),
                )
                .await;

            return;
        };

        let manager = songbird::get(ctx)
            .await
            .expect("Songbird Voice client was not initialized")
            .clone();

        let Some(call_lock) = manager.get(guild_id) else {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "I'm not in a voice channel.",
                )
                .await;

            return;
        };

        {
            let mut sounds = self.current_sounds.write().await;

            if let Some(previous) = sounds.remove(&guild_id) {
                let _ = previous.stop();
            }
        }

        let track = {
            let mut call = call_lock.lock().await;

            call.play_input(
                songbird::input::File::new(sound_file).into()
            )
        };

        {
            let mut sounds = self.current_sounds.write().await;
            sounds.insert(guild_id, track);
        }

        let _ = msg
            .channel_id
            .say(
                &ctx.http,
                format!(
                    "Playing `{}`.",
                    requested_name
                ),
            )
            .await;
    }

    async fn handle_stop_sound(
        &self,
        ctx: &Context,
        msg: &Message,
    ) {
        let Some(guild_id) = msg.guild_id else {
            return;
        };

        let mut sounds = self.current_sounds.write().await;

        match sounds.remove(&guild_id) {
            Some(track) => {
                match track.stop() {
                    Ok(_) => {
                        let _ = msg
                            .channel_id
                            .say(
                                &ctx.http,
                                "Stopped.",
                            )
                            .await;
                    }

                    Err(error) => {
                        eprintln!(
                            "Failed to stop sound in {}: {}",
                            guild_id,
                            error
                        );

                        let _ = msg
                            .channel_id
                            .say(
                                &ctx.http,
                                "I couldn't stop the sound.",
                            )
                            .await;
                    }
                }
            }

            None => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Nothing is playing.",
                    )
                    .await;
            }
        }
    }
}

async fn run_sleep_scheduler(
    http: Arc<serenity::http::Http>,
    schedule: SleepSchedule,
    sleeping_users: SleepingUsers,
) {
    loop {
        let now = Utc::now();

        let Some(next_run) =
            next_sleep_time(&schedule, now)
        else {
            eprintln!(
                "Could not calculate next sleep time for {:02}:{:02} {}",
                schedule.hour,
                schedule.minute,
                schedule.timezone
            );

            tokio::time::sleep(
                std::time::Duration::from_secs(60),
            )
            .await;

            continue;
        };

        let wait = next_run
            .signed_duration_since(now)
            .to_std()
            .unwrap_or_else(|_| {
                std::time::Duration::from_secs(0)
            });

        println!(
            "Next scheduled bedtime: {}",
            next_run
        );

        tokio::time::sleep(wait).await;

        {
            let mut users = sleeping_users.write().await;
            users.insert((schedule.guild_id, schedule.user_id));
        }

        match schedule
            .guild_id
            .disconnect_member(&http, schedule.user_id)
            .await
        {
            Ok(_) => {
                println!(
                    "Disconnected {} from voice for scheduled bedtime",
                    schedule.user_id
                );
            }

            Err(error) => {
                eprintln!(
                    "Could not disconnect {} from voice: {}",
                    schedule.user_id,
                    error
                );
            }
        }

        tokio::time::sleep(
            std::time::Duration::from_secs(5),
        )
        .await;
    }
}

fn next_sleep_time(
    schedule: &SleepSchedule,
    now_utc: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let local_now = now_utc.with_timezone(&schedule.timezone);

    let mut date = local_now.date_naive();

    for _ in 0..3 {
        let naive = date.and_hms_opt(
            schedule.hour,
            schedule.minute,
            0,
        )?;

        let local_result =
            schedule.timezone.from_local_datetime(&naive);

        let candidate = match local_result {
            LocalResult::Single(datetime) => datetime,

            LocalResult::Ambiguous(first, second) => {
                first.min(second)
            }

            LocalResult::None => {
                date += ChronoDuration::days(1);
                continue;
            }
        };

        let candidate_utc =
            candidate.with_timezone(&Utc);

        if candidate_utc > now_utc {
            return Some(candidate_utc);
        }

        date += ChronoDuration::days(1);
    }

    None
}

fn parse_voice_command(
    msg: &Message,
    bot_id: UserId,
) -> Option<VoiceCommand> {
    let mut content = msg.content.clone();

    content = content.replace(
        &format!("<@{}>", bot_id.get()),
        "",
    );

    content = content.replace(
        &format!("<@!{}>", bot_id.get()),
        "",
    );

    let command = content.trim();

    let lower = command.to_lowercase();

    if lower == "join"
        || lower == "come"
        || lower == "komm"
    {
        return Some(VoiceCommand::JoinAuthor);
    }

    if lower == "leave"
        || lower == "disconnect"
        || lower == "go away"
    {
        return Some(VoiceCommand::Leave);
    }

    if lower == "stop" {
        return Some(VoiceCommand::StopSound);
    }

    if let Some(sound) = command.strip_prefix("sound ")
        .or_else(|| command.strip_prefix("play "))
    {
        let sound = sound.trim().to_lowercase();

        if !sound.is_empty() {
            return Some(VoiceCommand::PlaySound(sound));
        }
    }

    if lower.starts_with("join ")
        || lower.starts_with("come ")
        || lower.starts_with("komm ")
    {
        if let Some(user) = msg
            .mentions
            .iter()
            .find(|user| user.id != bot_id)
        {
            return Some(VoiceCommand::JoinUser(user.id));
        }

        if let Some(channel) = msg.mention_channels.first() {
            return Some(VoiceCommand::JoinChannel(channel.id));
        }
    }

    None
}

fn build_prompt(
    msg: &Message,
    bot_id: serenity::all::UserId,
) -> String {
    let mut content = msg.content.clone();

    content = content.replace(
        &format!("<@{}>", bot_id.get()),
        "",
    );

    content = content.replace(
        &format!("<@!{}>", bot_id.get()),
        "",
    );

    for user in &msg.mentions {
        if user.id == bot_id {
            continue;
        }

        let name = user
            .global_name
            .as_deref()
            .unwrap_or(&user.name);

        content = content.replace(
            &format!("<@{}>", user.id.get()),
            name,
        );

        content = content.replace(
            &format!("<@!{}>", user.id.get()),
            name,
        );
    }

    content.trim().to_string()
}

fn split_message(
    text: &str,
    max_len: usize,
) -> Vec<String> {
    if text.chars().count() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let extra = if current.is_empty() {
            word.len()
        } else {
            word.len() + 1
        };

        if current.len() + extra > max_len {
            if !current.is_empty() {
                chunks.push(current);
                current = String::new();
            }
        }

        if !current.is_empty() {
            current.push(' ');
        }

        current.push_str(word);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}