use anyhow::Result;
use colored::Colorize;
use std::time::Instant;

/// Test questions: simple, medium, complex
const QUESTIONS: &[(&str, &str)] = &[
    ("simple", "What is the capital of France? Answer in one sentence."),
    ("medium", "Explain the difference between TCP and UDP. Include when you would use each. Keep it under 100 words."),
    ("complex", "Write a Rust function that takes a Vec<i32> and returns the longest increasing subsequence. Include the function signature, implementation, and a brief explanation of the algorithm's time complexity."),
];

pub async fn run(ollama_url: &str) -> Result<()> {
    println!("{}", "╔══════════════════════════════════════════════════╗".cyan());
    println!("{}", "║        OpenClaw Model Arena — RTX 5060 Ti       ║".cyan());
    println!("{}", "╚══════════════════════════════════════════════════╝".cyan());
    println!();

    // Discover available models
    let client = reqwest::Client::new();
    let resp = client.get(format!("{}/api/tags", ollama_url))
        .send().await?
        .json::<serde_json::Value>().await?;

    let models: Vec<String> = resp["models"].as_array()
        .map(|arr| arr.iter()
            .filter_map(|m| m["name"].as_str().map(String::from))
            .collect())
        .unwrap_or_default();

    if models.is_empty() {
        anyhow::bail!("No models found at {}. Pull models first.", ollama_url);
    }

    println!("  {} models found: {}", models.len(), models.join(", ").yellow());
    println!();

    // Results table
    let mut results: Vec<ArenaResult> = Vec::new();

    for (difficulty, question) in QUESTIONS {
        println!("{}", format!("── {} ──", difficulty.to_uppercase()).bold());
        println!("  Q: {}", question.dimmed());
        println!();

        for model in &models {
            print!("  {} ... ", model.bold());

            let start = Instant::now();
            match query_ollama(&client, ollama_url, model, question).await {
                Ok((response, tokens)) => {
                    let elapsed = start.elapsed();
                    let ms = elapsed.as_millis() as u64;
                    let tok_per_sec = if elapsed.as_secs_f64() > 0.0 {
                        tokens as f64 / elapsed.as_secs_f64()
                    } else {
                        0.0
                    };

                    println!("{} {}ms, {} tok, {:.1} tok/s",
                        "✓".green(), ms, tokens, tok_per_sec);

                    // Truncate response for display
                    let preview = if response.len() > 200 {
                        format!("{}...", &response[..197])
                    } else {
                        response.clone()
                    };
                    println!("  {}", preview.dimmed());
                    println!();

                    // Write to Postgres if available
                    if let Some(pool) = openclaw_db::pool() {
                        let id = uuid::Uuid::new_v4();
                        let _ = openclaw_db::llm_log::record_llm_call(
                            pool, &id,
                            Some(&format!("arena:{}", difficulty)),
                            model, 1,
                            1, 0, false,
                            Some(&response), None,
                            0, &[], 0, tokens as i32, tokens as i32,
                            ms as i32, None,
                        ).await;
                    }

                    results.push(ArenaResult {
                        model: model.clone(),
                        difficulty: difficulty.to_string(),
                        latency_ms: ms,
                        tokens,
                        tok_per_sec,
                        error: None,
                    });
                }
                Err(e) => {
                    let ms = start.elapsed().as_millis() as u64;
                    println!("{} {}ms — {}", "✗".red(), ms, e);
                    println!();

                    results.push(ArenaResult {
                        model: model.clone(),
                        difficulty: difficulty.to_string(),
                        latency_ms: ms,
                        tokens: 0,
                        tok_per_sec: 0.0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
    }

    // Summary table
    println!();
    println!("{}", "══════════════════════════════════════════════════".cyan());
    println!("{}", "  RESULTS SUMMARY".bold());
    println!("{}", "══════════════════════════════════════════════════".cyan());
    println!();
    println!("  {:<28} {:>6} {:>6} {:>8} {:>10}",
        "Model".bold(), "Easy".bold(), "Med".bold(), "Hard".bold(), "Avg tok/s".bold());
    println!("  {}", "─".repeat(62));

    for model in &models {
        let model_results: Vec<&ArenaResult> = results.iter()
            .filter(|r| &r.model == model && r.error.is_none())
            .collect();

        if model_results.is_empty() {
            println!("  {:<28} {}", model, "FAILED".red());
            continue;
        }

        let easy = model_results.iter().find(|r| r.difficulty == "simple")
            .map(|r| format!("{}ms", r.latency_ms)).unwrap_or("-".into());
        let med = model_results.iter().find(|r| r.difficulty == "medium")
            .map(|r| format!("{}ms", r.latency_ms)).unwrap_or("-".into());
        let hard = model_results.iter().find(|r| r.difficulty == "complex")
            .map(|r| format!("{}ms", r.latency_ms)).unwrap_or("-".into());
        let avg_tps: f64 = model_results.iter().map(|r| r.tok_per_sec).sum::<f64>()
            / model_results.len() as f64;

        println!("  {:<28} {:>6} {:>6} {:>8} {:>8.1} t/s",
            model, easy, med, hard, avg_tps);
    }

    println!();

    Ok(())
}

async fn query_ollama(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    prompt: &str,
) -> Result<(String, u64)> {
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "stream": false,
        "options": {
            "num_predict": 512,
        }
    });

    let resp = client.post(format!("{}/api/generate", base_url))
        .json(&body)
        .timeout(std::time::Duration::from_secs(120))
        .send().await?
        .json::<serde_json::Value>().await?;

    let response = resp["response"].as_str()
        .unwrap_or("(empty response)")
        .to_string();

    let tokens = resp["eval_count"].as_u64().unwrap_or(0);

    if let Some(err) = resp["error"].as_str() {
        anyhow::bail!("{}", err);
    }

    Ok((response, tokens))
}

struct ArenaResult {
    model: String,
    difficulty: String,
    latency_ms: u64,
    tokens: u64,
    tok_per_sec: f64,
    error: Option<String>,
}
