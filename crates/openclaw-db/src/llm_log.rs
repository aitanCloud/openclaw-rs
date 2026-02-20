use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Persist an LLM call to the database
pub async fn record_llm_call(
    pool: &PgPool,
    id: &Uuid,
    session_key: Option<&str>,
    model: &str,
    provider_attempt: i16,
    messages_count: i32,
    request_tokens_est: i32,
    streaming: bool,
    response_content: Option<&str>,
    response_reasoning: Option<&str>,
    response_tool_calls: i32,
    tool_call_names: &[String],
    usage_prompt_tokens: i32,
    usage_completion_tokens: i32,
    usage_total_tokens: i32,
    latency_ms: i32,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO llm_calls (
            id, session_key, model, provider_attempt, messages_count,
            request_tokens_est, streaming, response_content, response_reasoning,
            response_tool_calls, tool_call_names, usage_prompt_tokens,
            usage_completion_tokens, usage_total_tokens, latency_ms, error
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)"
    )
    .bind(id)
    .bind(session_key)
    .bind(model)
    .bind(provider_attempt)
    .bind(messages_count)
    .bind(request_tokens_est)
    .bind(streaming)
    .bind(response_content)
    .bind(response_reasoning)
    .bind(response_tool_calls)
    .bind(tool_call_names)
    .bind(usage_prompt_tokens)
    .bind(usage_completion_tokens)
    .bind(usage_total_tokens)
    .bind(latency_ms)
    .bind(error)
    .execute(pool)
    .await?;

    Ok(())
}
