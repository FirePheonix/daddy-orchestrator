use crate::models::UsageStats;

#[derive(Debug, Clone, Copy)]
struct PricingRule {
    model_prefix: &'static str,
    input_per_million: f64,
    output_per_million: f64,
    cache_read_per_million: f64,
    cache_write_per_million: f64,
}

const CODEX_PRICING: &[PricingRule] = &[
    PricingRule {
        model_prefix: "gpt-4.1",
        input_per_million: 2.0,
        output_per_million: 8.0,
        cache_read_per_million: 0.5,
        cache_write_per_million: 0.0,
    },
    PricingRule {
        model_prefix: "o4-mini",
        input_per_million: 1.1,
        output_per_million: 4.4,
        cache_read_per_million: 0.275,
        cache_write_per_million: 0.0,
    },
];

const CLAUDE_PRICING: &[PricingRule] = &[
    PricingRule {
        model_prefix: "claude-opus-4",
        input_per_million: 15.0,
        output_per_million: 75.0,
        cache_read_per_million: 1.5,
        cache_write_per_million: 18.75,
    },
    PricingRule {
        model_prefix: "claude-sonnet-5",
        input_per_million: 3.0,
        output_per_million: 15.0,
        cache_read_per_million: 0.3,
        cache_write_per_million: 3.75,
    },
];

const OPENCODE_PRICING: &[PricingRule] = &[
    PricingRule {
        model_prefix: "anthropic/claude-sonnet-5",
        input_per_million: 3.0,
        output_per_million: 15.0,
        cache_read_per_million: 0.3,
        cache_write_per_million: 3.75,
    },
    PricingRule {
        model_prefix: "openai/gpt-4.1-mini",
        input_per_million: 0.4,
        output_per_million: 1.6,
        cache_read_per_million: 0.1,
        cache_write_per_million: 0.0,
    },
];

// Compute a fallback USD cost estimate from provider/model pricing tables.
pub fn compute_cost(provider: &str, model: &str, usage: &UsageStats) -> Option<f64> {
    let rule = pricing_rule(provider, model)?;
    Some(
        cost_for_tokens(usage.input_tokens, rule.input_per_million)
            + cost_for_tokens(usage.output_tokens, rule.output_per_million)
            + cost_for_tokens(usage.cache_read_tokens, rule.cache_read_per_million)
            + cost_for_tokens(usage.cache_write_tokens, rule.cache_write_per_million),
    )
}

// Resolve the best pricing rule for a provider and model pair.
fn pricing_rule(provider: &str, model: &str) -> Option<PricingRule> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    pricing_rules(provider)
        .iter()
        .copied()
        .find(|rule| model.starts_with(rule.model_prefix))
}

// Return the pricing rule table for the selected provider.
fn pricing_rules(provider: &str) -> &'static [PricingRule] {
    match provider {
        "codex" => CODEX_PRICING,
        "claude" => CLAUDE_PRICING,
        "opencode" => OPENCODE_PRICING,
        _ => &[],
    }
}

// Convert a token count and per-million price into a USD subtotal.
fn cost_for_tokens(tokens: u64, per_million: f64) -> f64 {
    (tokens as f64 / 1_000_000.0) * per_million
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Estimate a cost for a known codex model and non-zero usage.
    fn compute_cost_uses_matching_pricing_rule() {
        let cost = compute_cost(
            "codex",
            "gpt-4.1",
            &UsageStats {
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.0,
            },
        )
        .unwrap();
        assert!(cost > 0.0);
    }

    #[test]
    // Return no estimate when the model is unknown to the pricing table.
    fn compute_cost_returns_none_for_unknown_model() {
        assert!(compute_cost("codex", "unknown-model", &UsageStats::default()).is_none());
    }
}
