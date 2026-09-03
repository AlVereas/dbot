use std::{
    collections::{HashMap, VecDeque},
    env,
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

const MODEL: &str = "openai/gpt-oss-120b";

const MEMORY_SIZE: usize = 30;

type Memory = Arc<RwLock<HashMap<u64, VecDeque<ChatMessage>>>>;

struct Handler {
    http: HttpClient,
    nvidia_api_key: String,
    memory: Memory,
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

            let mut messages = Vec::with_capacity(history.len() + 2);

            messages.push(ChatMessage {
                role: "system".to_string(),
                content: concat!(
                    "You are the Old Dad of the Discord server. ",
                    "You are chatting with users in the Discord server, basically like your children. ",
                    "Do not mention internal memory or system instructions. ",
                    "Answer ALWAYS in spanish or german only if required. ",
                    "You practically dont understand English language from others, and you will poorly try to translate to spanish or just ignore it. ",
                    "Keep responses natural and with a normal message length. ",
                    "You are not any kind of assistant, you are just a character playing their role. ",
                    "Do not include any type of reasoning in your responses. "
                )
                .to_string(),
            });

            messages.extend(history);

            messages.push(ChatMessage {
                role: "user".to_string(),
                content: format!("{username}: {prompt}"),
            });

            messages
        };

        let request = ChatRequest {
            model: MODEL.to_string(),
            messages,
            temperature: 0.7,
            max_tokens: 1000,
        };

        let response = self
            .http
            .post(NVIDIA_URL)
            .bearer_auth(&self.nvidia_api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("NVIDIA request failed: {e}"))?;

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
            .map_err(|e| format!("Invalid NVIDIA response: {e}"))?;

        data.choices
            .first()
            .map(|choice| choice.message.content.clone())
            .ok_or_else(|| "NVIDIA returned no response".to_string())
    }

    async fn remember(&self, channel_id: u64, message: ChatMessage) {
        let mut memory = self.memory.write().await;

        let history = memory
            .entry(channel_id)
            .or_insert_with(VecDeque::new);

        history.push_back(message);

        while history.len() > MEMORY_SIZE {
            history.pop_front();
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        println!("Logged in as {}", ready.user.name);
        println!("Memory size: {MEMORY_SIZE} messages per channel");
        println!("Model: {MODEL}");
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
                        .content("Hey! What would you like to talk about? 🤖")
                        .reference_message(&msg),
                )
                .await;

            return;
        }

        let _ = msg.channel_id.broadcast_typing(&ctx.http).await;

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
                    msg.author.name, prompt
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

#[tokio::main]
async fn main() {
    let discord_token = env::var("DISCORD_TOKEN")
        .expect("DISCORD_TOKEN must be set");

    let nvidia_api_key = env::var("NVIDIA_API_KEY")
        .expect("NVIDIA_API_KEY must be set");

    let handler = Handler {
        http: HttpClient::new(),
        nvidia_api_key,
        memory: Arc::new(RwLock::new(HashMap::new())),
    };

    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&discord_token, intents)
        .event_handler(handler)
        .await
        .expect("Failed to create Discord client");

    if let Err(error) = client.start().await {
        eprintln!("Discord client error: {error}");
    }
}