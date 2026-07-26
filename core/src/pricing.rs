//! Model pricing and cost attribution.
//!
//! Rates are per million tokens, from Anthropic's published pricing. Cache is
//! priced as a multiple of the model's base input rate: a 5-minute write costs
//! 1.25×, a one-hour write 2×, and a read 0.1×. That read discount is why cache
//! accounting dominates the cost of an agentic session — a run that re-sends the
//! same conversation every turn pays a tenth of list price for almost all of it.
//!
//! An unknown model yields no cost at all rather than zero. A zero would render
//! as "this call was free", which is a wrong answer; absent renders as unknown,
//! which is the true one.

/// Bumping this re-prices existing generations without re-parsing them.
pub const PRICING_VERSION: &str = "2026-07-26.1";

/// Multiplier applied to the base input rate for a 5-minute cache write.
const CACHE_WRITE_5M: f64 = 1.25;
/// Multiplier for a one-hour cache write.
const CACHE_WRITE_1H: f64 = 2.00;
/// Multiplier for reading from cache.
const CACHE_READ: f64 = 0.10;

const PER_MTOK: f64 = 1_000_000.0;

/// How a table entry matches a model id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    /// The id must match exactly.
    Exact,
    /// The id must start with this string — covers dated snapshots such as
    /// `claude-haiku-4-5-20251001`.
    Prefix,
}

/// Promotional pricing and the instant it stops applying.
#[derive(Debug, Clone, Copy)]
struct Promo {
    input_per_mtok: f64,
    output_per_mtok: f64,
    /// RFC3339 instant; compared lexicographically against the capture time.
    until: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ModelPrice {
    id: &'static str,
    match_kind: Match,
    input_per_mtok: f64,
    output_per_mtok: f64,
    promo: Option<Promo>,
}

/// Published list prices. Ordered longest-id-first so a more specific prefix
/// wins over a shorter one.
const PRICES: &[ModelPrice] = &[
    ModelPrice {
        id: "claude-haiku-4-5",
        match_kind: Match::Prefix,
        input_per_mtok: 1.00,
        output_per_mtok: 5.00,
        promo: None,
    },
    ModelPrice {
        id: "claude-sonnet-4-6",
        match_kind: Match::Prefix,
        input_per_mtok: 3.00,
        output_per_mtok: 15.00,
        promo: None,
    },
    ModelPrice {
        id: "claude-sonnet-5",
        match_kind: Match::Prefix,
        input_per_mtok: 3.00,
        output_per_mtok: 15.00,
        // Introductory rate at launch; captures before this instant are priced
        // at what they actually cost, not at today's list price.
        promo: Some(Promo {
            input_per_mtok: 2.00,
            output_per_mtok: 10.00,
            until: "2026-09-01T00:00:00Z",
        }),
    },
    ModelPrice {
        id: "claude-opus-4-6",
        match_kind: Match::Prefix,
        input_per_mtok: 5.00,
        output_per_mtok: 25.00,
        promo: None,
    },
    ModelPrice {
        id: "claude-opus-4-7",
        match_kind: Match::Prefix,
        input_per_mtok: 5.00,
        output_per_mtok: 25.00,
        promo: None,
    },
    ModelPrice {
        id: "claude-opus-4-8",
        match_kind: Match::Prefix,
        input_per_mtok: 5.00,
        output_per_mtok: 25.00,
        promo: None,
    },
    ModelPrice {
        id: "claude-opus-5",
        match_kind: Match::Prefix,
        input_per_mtok: 5.00,
        output_per_mtok: 25.00,
        promo: None,
    },
    ModelPrice {
        id: "claude-fable-5",
        match_kind: Match::Exact,
        input_per_mtok: 10.00,
        output_per_mtok: 50.00,
        promo: None,
    },
    ModelPrice {
        id: "claude-mythos-5",
        match_kind: Match::Exact,
        input_per_mtok: 10.00,
        output_per_mtok: 50.00,
        promo: None,
    },
];

/// Token counts a call reported, as stored on a derived generation.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tokens {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_creation_5m: Option<i64>,
    pub cache_creation_1h: Option<i64>,
    /// Total cache writes, used when the 5m/1h split is unknown.
    pub cache_creation_total: Option<i64>,
}

/// What one call cost, broken out so the UI can attribute it.
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    pub input_usd: f64,
    pub output_usd: f64,
    pub cache_write_usd: f64,
    pub cache_read_usd: f64,
    pub total_usd: f64,
    /// What the same call would have cost with no caching at all. The gap
    /// between this and `total_usd` is the cache's contribution.
    pub uncached_equivalent_usd: f64,
    /// The table entry that priced this call.
    pub model_id: &'static str,
}

impl Cost {
    /// Money the cache saved on this call. Never negative: a write-heavy call
    /// can cost more than its uncached equivalent, and reporting that as a
    /// negative saving reads as a refund rather than an overhead.
    pub fn savings_usd(&self) -> f64 {
        (self.uncached_equivalent_usd - self.total_usd).max(0.0)
    }
}

impl Tokens {
    /// Did the provider report any usage at all?
    ///
    /// A call with no counters is not a free call — it is one whose usage we
    /// never captured. Pricing it produces $0.00, which reads as "free" and is
    /// the wrong answer.
    fn reported(&self) -> bool {
        self.input.is_some()
            || self.output.is_some()
            || self.cache_read.is_some()
            || self.cache_creation_5m.is_some()
            || self.cache_creation_1h.is_some()
            || self.cache_creation_total.is_some()
    }
}

/// Price one call. `at` is the capture time, used to resolve promotional rates.
///
/// Returns `None` when the model is absent from the pricing table, or when the
/// call reported no usage at all — in both cases the caller must leave cost
/// unset rather than substituting zero.
pub fn price(model: Option<&str>, at: &str, tokens: &Tokens) -> Option<Cost> {
    if !tokens.reported() {
        return None;
    }
    let entry = lookup(model?)?;
    let (input_rate, output_rate) = match entry.promo {
        Some(promo) if at < promo.until => (
            promo.input_per_mtok / PER_MTOK,
            promo.output_per_mtok / PER_MTOK,
        ),
        _ => (
            entry.input_per_mtok / PER_MTOK,
            entry.output_per_mtok / PER_MTOK,
        ),
    };

    let count = |value: Option<i64>| value.unwrap_or(0).max(0) as f64;

    // Prefer the reported TTL split; fall back to the total, priced at the
    // cheaper 5-minute rate so an unknown split never inflates the bill.
    let (write_5m, write_1h) = match (tokens.cache_creation_5m, tokens.cache_creation_1h) {
        (None, None) => (count(tokens.cache_creation_total), 0.0),
        (five, hour) => (count(five), count(hour)),
    };

    let input_usd = count(tokens.input) * input_rate;
    let output_usd = count(tokens.output) * output_rate;
    let cache_write_usd =
        write_5m * input_rate * CACHE_WRITE_5M + write_1h * input_rate * CACHE_WRITE_1H;
    let cache_read_usd = count(tokens.cache_read) * input_rate * CACHE_READ;

    // Without caching every context token would have been billed at the full
    // input rate, which is the comparison that makes the saving meaningful.
    let uncached_input = count(tokens.input) + count(tokens.cache_read) + write_5m + write_1h;

    Some(Cost {
        input_usd,
        output_usd,
        cache_write_usd,
        cache_read_usd,
        total_usd: input_usd + output_usd + cache_write_usd + cache_read_usd,
        uncached_equivalent_usd: uncached_input * input_rate + output_usd,
        model_id: entry.id,
    })
}

/// Is this model priced at all? Used to raise a data-quality signal rather than
/// silently reporting an incomplete total.
pub fn is_known(model: Option<&str>) -> bool {
    model.and_then(lookup).is_some()
}

fn lookup(model: &str) -> Option<&'static ModelPrice> {
    PRICES
        .iter()
        .filter(|entry| match entry.match_kind {
            Match::Exact => entry.id == model,
            Match::Prefix => model.starts_with(entry.id),
        })
        // The longest match wins, so `claude-opus-4-8` beats a hypothetical
        // shorter `claude-opus` entry.
        .max_by_key(|entry| entry.id.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-07-26T00:00:00Z";

    #[test]
    fn matches_the_rates_the_provider_actually_billed() {
        // Cross-check against a real captured generation: 1 input token and 291
        // output tokens on claude-opus-5 were billed $0.000005 and $0.007275.
        let cost = price(
            Some("claude-opus-5"),
            NOW,
            &Tokens {
                input: Some(1),
                output: Some(291),
                ..Default::default()
            },
        )
        .unwrap();
        assert!((cost.input_usd - 0.000005).abs() < 1e-12);
        assert!((cost.output_usd - 0.007275).abs() < 1e-12);
    }

    #[test]
    fn cache_writes_are_priced_by_ttl_and_reads_at_a_tenth() {
        let cost = price(
            Some("claude-opus-5"),
            NOW,
            &Tokens {
                input: Some(0),
                output: Some(0),
                cache_read: Some(1_000_000),
                cache_creation_5m: Some(1_000_000),
                cache_creation_1h: Some(1_000_000),
                cache_creation_total: None,
            },
        )
        .unwrap();
        // 1 MTok at $5 base: 5m write 1.25× + 1h write 2× = $16.25.
        assert!((cost.cache_write_usd - 16.25).abs() < 1e-9);
        // A read is a tenth of base.
        assert!((cost.cache_read_usd - 0.50).abs() < 1e-9);
    }

    #[test]
    fn an_unknown_split_is_priced_at_the_cheaper_ttl() {
        let cost = price(
            Some("claude-opus-5"),
            NOW,
            &Tokens {
                cache_creation_total: Some(1_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        // Assuming the expensive 1h rate would overstate the bill.
        assert!((cost.cache_write_usd - 6.25).abs() < 1e-9);
    }

    #[test]
    fn cache_reads_show_up_as_savings() {
        let cost = price(
            Some("claude-opus-5"),
            NOW,
            &Tokens {
                input: Some(10),
                output: Some(161),
                cache_read: Some(86_515),
                cache_creation_1h: Some(10_338),
                ..Default::default()
            },
        )
        .unwrap();
        // Reads cost a tenth of what they would have uncached, so the saving is
        // real and large; writes are the offsetting overhead.
        assert!(cost.savings_usd() > 0.0);
        assert!(cost.uncached_equivalent_usd > cost.total_usd);
    }

    #[test]
    fn savings_never_report_as_negative() {
        // A pure cache-write call costs more than its uncached equivalent.
        let cost = price(
            Some("claude-opus-5"),
            NOW,
            &Tokens {
                cache_creation_1h: Some(100_000),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(cost.total_usd > cost.uncached_equivalent_usd);
        assert_eq!(cost.savings_usd(), 0.0);
    }

    #[test]
    fn a_dated_snapshot_matches_its_family_by_prefix() {
        let cost = price(
            Some("claude-haiku-4-5-20251001"),
            NOW,
            &Tokens {
                input: Some(1_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(cost.model_id, "claude-haiku-4-5");
        assert!((cost.input_usd - 1.00).abs() < 1e-9);
    }

    #[test]
    fn promotional_rates_apply_only_inside_their_window() {
        let tokens = Tokens {
            input: Some(1_000_000),
            ..Default::default()
        };
        let during = price(Some("claude-sonnet-5"), "2026-07-26T00:00:00Z", &tokens).unwrap();
        let after = price(Some("claude-sonnet-5"), "2026-10-01T00:00:00Z", &tokens).unwrap();
        assert!((during.input_usd - 2.00).abs() < 1e-9);
        assert!((after.input_usd - 3.00).abs() < 1e-9);
    }

    #[test]
    fn an_unknown_model_has_no_cost_rather_than_a_zero_one() {
        let tokens = Tokens {
            input: Some(10),
            ..Default::default()
        };
        assert!(price(Some("some-other-model"), NOW, &tokens).is_none());
        assert!(price(None, NOW, &tokens).is_none());
        assert!(!is_known(Some("some-other-model")));
        assert!(is_known(Some("claude-opus-5")));
    }

    #[test]
    fn a_call_with_no_reported_usage_has_no_cost_rather_than_a_zero_one() {
        // The response was never captured. Pricing this at $0.00 would report a
        // call that certainly cost money as free.
        assert!(price(Some("claude-opus-5"), NOW, &Tokens::default()).is_none());
        // A genuine zero is different from an absent one and must still price.
        let genuine_zero = price(
            Some("claude-opus-5"),
            NOW,
            &Tokens {
                input: Some(0),
                output: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(genuine_zero.total_usd, 0.0);
    }
}
