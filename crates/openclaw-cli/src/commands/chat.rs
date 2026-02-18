use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    total_tokens: Option<u32>,
}

pub async fn run(
    message: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Result<()> {
    let client = reqwest::Client::new();

    let request = ChatRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: message.to_string(),
        }],
        max_tokens: 256,
    };

    let t_start = Instant::now();

    let response = client
        .post(format!("{}/chat/completions", base_url))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .context("Failed to send request to LLM API")?;

    let t_response = t_start.elapsed();

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("API returned {}: {}", status, body);
    }

    let chat_response: ChatResponse = response
        .json()
        .await
        .context("Failed to parse LLM response")?;

    let t_total = t_start.elapsed();

    // Print response
    if let Some(choice) = chat_response.choices.first() {
        println!("{}", choice.message.content);
    }

    // Print timing
    println!();
    println!("{}", "─".repeat(40).dimmed());
    println!(
        "  {} {:.0}ms",
        "Response time:".dimmed(),
        t_response.as_secs_f64() * 1000.0
    );
    println!(
        "  {} {:.0}ms",
        "Total time:".dimmed(),
        t_total.as_secs_f64() * 1000.0
    );
    println!(
        "  {} {} / {}",
        "Model:".dimmed(),
        model.cyan(),
        base_url.dimmed()
    );

    if let Some(usage) = chat_response.choices.first().and_then(|_| chat_response.usage.as_ref()) {
        if let (Some(prompt), Some(completion)) = (usage.prompt_tokens, usage.completion_tokens) {
            println!(
                "  {} {} prompt + {} completion = {} total",
                "Tokens:".dimmed(),
                prompt,
                completion,
                usage.total_tokens.unwrap_or(prompt + completion)
            );
        }
    }

    Ok(())
}
