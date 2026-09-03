use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use serenity::{
    async_trait,
    builder::CreateMessage,
    model::{
        channel::Message,
        gateway::Ready,
    },
    prelude::*,
};
use tokio::sync::RwLock;

const NVIDIA_URL: &str =
    "https://integrate.api.nvidia.com/v1/chat/completions";

const DEFAULT_MODEL: &str = "openai/gpt-oss-120b";

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

type Memory = Arc<RwLock<HashMap<u64, VecDeque<ChatMessage>>>>;

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

    pub fn system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = system_prompt.into();
        self
    }


    }

    pub struct Bot {
        config: BotConfig,
        http: HttpClient,
        memory: Memory,
    }

    impl Bot {
        pub fn new(config: BotConfig) -> Self {
        Self {
            config,
            http: HttpClient::new(),
            memory: Arc::new(RwLock::new(HashMap::new())),
        }
    }


    pub async fn run(self) -> Result<(), serenity::Error> {
        let handler = Handler {
            http: self.http,
            config: self.config.clone(),
            memory: self.memory,
        };

        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;

        let mut client = Client::builder(
            &self.config.discord_token,
            intents,
        )
        .event_handler(handler)
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

struct Handler {
    http: HttpClient,
    config: BotConfig,
    memory: Memory,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        println!("Logged in as {}", ready.user.name);
        println!("Model: {}", self.config.model);
        println!(
        "Memory: {} messages per channel",
        self.config.memory_size
    );
}


async fn message(&self, ctx: Context, msg: Message) {
    if msg.author.bot {
        return;
    }

    let bot_id = ctx.cache.current_user().id;

    if !msg.mentions.iter().any(|user| user.id == bot_id) {
        return;
    }

    let prompt = msg
        .content
        .replace(&format!("<@{}>", bot_id), "")
        .replace(&format!("<@!{}>", bot_id), "")
        .trim()
        .to_string();

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


}

fn split_message(text: &str, max_len: usize) -> Vec<String> {
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
