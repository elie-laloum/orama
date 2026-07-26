//! Model pricing and cost attribution.
//!
//! Rates come from the model catalogue ([`crate::catalog`]), which mirrors
//! models.dev. What remains here is the part a published rate card cannot
//! express: how cache tokens are priced relative to the base input rate, which
//! price band a given context size falls into, and the handful of rates the
//! catalogue does not carry.
//!
//! Anthropic's cache is priced as a multiple of the base input rate — a
//! 5-minute write costs 1.25×, a one-hour write 2×, and a read 0.1×. That read
//! discount is why cache accounting dominates the cost of an agentic session: a
//! run that re-sends the same conversation every turn pays a tenth of list price
//! for almost all of it. The catalogue publishes the read and 5-minute write
//! rates directly; it has no concept of a one-hour write, so that one is still
//! derived.
//!
//! An unknown model yields no cost at all rather than zero. A zero would render
//! as "this call was free", which is a wrong answer; absent renders as unknown,
//! which is the true one.

/// Version of the pricing *rules* — the cache multipliers, the tier boundary,
/// and the local supplement below. Bump it when that logic changes.
///
/// This is only half the story. What actually identifies a price is this plus
/// the catalogue snapshot, which is why [`pricing_version`] composes the two and
/// is what gets stamped onto a row.
pub const PRICING_RULES_VERSION: &str = "2026-07-27.1";

/// Multiplier applied to the base input rate for a 5-minute cache write, when
/// the catalogue does not publish one.
const CACHE_WRITE_5M: f64 = 1.25;
/// Multiplier for a one-hour cache write. Never published — always derived.
const CACHE_WRITE_1H: f64 = 2.00;
/// Multiplier for reading from cache, when the catalogue does not publish one.
const CACHE_READ: f64 = 0.10;

const PER_MTOK: f64 = 1_000_000.0;

/// How a supplement entry matches a model id.
///
/// [`crate::catalog::Catalog::resolve`] deliberately refuses prefix matching,
/// and this is not a contradiction: there, a prefix is tested against 5,756 ids
/// nobody here curated, 926 of which are priced differently from the shorter
/// sibling that prefixes them. Here it is tested against the two entries
/// directly below, chosen by hand, where `claude-sonnet-5-20260601` matching
/// `claude-sonnet-5` is the whole point and there is no sibling to collide with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    /// The id must match exactly.
    Exact,
    /// The id must start with this string — covers dated snapshots such as
    /// `claude-sonnet-5-20260601`.
    Prefix,
}

/// A rate the catalogue cannot give us.
///
/// Two jobs, both of them things a *current* price card structurally cannot
/// express:
///
/// - **Historical.** models.dev publishes today's prices and keeps no history.
///   A capture from a promotional window was billed at the promotional rate, and
///   re-deriving it later must not re-price it at whatever is current.
/// - **Gap-fill.** A model the catalogue has never listed would otherwise lose
///   its price the moment this module stopped hard-coding one.
///
/// This is the only pricing data still maintained by hand, and it is meant to
/// stay this short.
#[derive(Debug, Clone, Copy)]
struct LocalRate {
    id: &'static str,
    match_kind: Match,
    input_per_mtok: f64,
    output_per_mtok: f64,
    /// RFC3339 instant, compared lexicographically against the capture time.
    /// `Some` overrides the catalogue for captures before it; `None` applies
    /// only when the catalogue does not know the model at all.
    until: Option<&'static str>,
}

const LOCAL: &[LocalRate] = &[
    // Introductory rate at launch. Captures from before this instant are priced
    // at what they actually cost, not at what the card says today.
    LocalRate {
        id: "claude-sonnet-5",
        match_kind: Match::Prefix,
        input_per_mtok: 2.00,
        output_per_mtok: 10.00,
        until: Some("2026-09-01T00:00:00Z"),
    },
    // Absent from models.dev entirely; without this it would silently stop
    // being priced.
    LocalRate {
        id: "claude-mythos-5",
        match_kind: Match::Exact,
        input_per_mtok: 10.00,
        output_per_mtok: 50.00,
        until: None,
    },
];

/// Identifies the rates in force: the rules version plus the catalogue snapshot.
///
/// Stamped onto every priced row and compared against `meta` on startup, so a
/// catalogue refresh actually re-prices what it invalidates instead of leaving
/// old rows quietly disagreeing with new ones.
pub fn pricing_version() -> String {
    let snapshot = crate::catalog::current()
        .map(|catalog| {
            let digest = &catalog.snapshot.digest;
            digest[..digest.len().min(12)].to_owned()
        })
        .unwrap_or_else(|| "none".to_owned());
    format!("{PRICING_RULES_VERSION}+cat:{snapshot}")
}

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
    /// The catalogue entry (or supplement) that priced this call.
    pub model_id: String,
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

/// The rates that apply to one call, and the id they came from.
struct Resolved {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write_5m: f64,
    cache_write_1h: f64,
    model_id: String,
}

/// Price one call.
///
/// `provider` is the dialect the call was captured under, used to scope the
/// catalogue lookup. `at` is the capture time, used to resolve historical rates.
/// `context_tokens` selects the price band for models that charge more above a
/// context size.
///
/// Returns `None` when the model is absent from the catalogue and the local
/// supplement, or when the call reported no usage at all — in both cases the
/// caller must leave cost unset rather than substituting zero.
pub fn price(provider: &str, model: Option<&str>, at: &str, tokens: &Tokens) -> Option<Cost> {
    if !tokens.reported() {
        return None;
    }
    let model = model?;

    let count = |value: Option<i64>| value.unwrap_or(0).max(0) as f64;

    // Prefer the reported TTL split; fall back to the total, priced at the
    // cheaper 5-minute rate so an unknown split never inflates the bill.
    let (write_5m, write_1h) = match (tokens.cache_creation_5m, tokens.cache_creation_1h) {
        (None, None) => (count(tokens.cache_creation_total), 0.0),
        (five, hour) => (count(five), count(hour)),
    };

    // Context is input plus cache read plus cache write — not `input_tokens`,
    // which is only the uncached remainder and can be single digits on a
    // 170k-token prompt. The price band depends on the whole prompt.
    let context_tokens =
        (count(tokens.input) + count(tokens.cache_read) + write_5m + write_1h) as i64;

    let rates = resolve(provider, model, at, context_tokens)?;

    let input_usd = count(tokens.input) * rates.input / PER_MTOK;
    let output_usd = count(tokens.output) * rates.output / PER_MTOK;
    let cache_write_usd =
        (write_5m * rates.cache_write_5m + write_1h * rates.cache_write_1h) / PER_MTOK;
    let cache_read_usd = count(tokens.cache_read) * rates.cache_read / PER_MTOK;

    // Without caching every context token would have been billed at the full
    // input rate, which is the comparison that makes the saving meaningful.
    let uncached_input = count(tokens.input) + count(tokens.cache_read) + write_5m + write_1h;

    Some(Cost {
        input_usd,
        output_usd,
        cache_write_usd,
        cache_read_usd,
        total_usd: input_usd + output_usd + cache_write_usd + cache_read_usd,
        uncached_equivalent_usd: uncached_input * rates.input / PER_MTOK + output_usd,
        model_id: rates.model_id,
    })
}

/// Work out the rates for a model, catalogue first.
fn resolve(provider: &str, model: &str, at: &str, context_tokens: i64) -> Option<Resolved> {
    let catalog = crate::catalog::current();
    let entry = catalog
        .as_ref()
        .and_then(|catalog| catalog.resolve(provider, model));
    let published = entry.and_then(|entry| entry.pricing.as_ref());

    // A historical rate beats the catalogue for captures inside its window; a
    // gap-fill only applies where the catalogue is silent.
    let historical = LOCAL
        .iter()
        .find(|local| matches(local, model) && local.until.is_some_and(|until| at < until));
    let gap_fill = LOCAL
        .iter()
        .find(|local| matches(local, model) && local.until.is_none());

    match (published, historical) {
        // The catalogue knows the model, but this capture predates a price
        // change we recorded. Base rates come from the supplement; the cache
        // multipliers still apply to that base.
        (_, Some(local)) => Some(Resolved {
            input: local.input_per_mtok,
            output: local.output_per_mtok,
            cache_read: local.input_per_mtok * CACHE_READ,
            cache_write_5m: local.input_per_mtok * CACHE_WRITE_5M,
            cache_write_1h: local.input_per_mtok * CACHE_WRITE_1H,
            model_id: entry
                .map(|e| e.id.clone())
                .unwrap_or_else(|| local.id.to_owned()),
        }),
        (Some(pricing), None) => {
            let band = pricing.at_context(context_tokens);
            Some(Resolved {
                input: band.input,
                output: band.output,
                cache_read: band.cache_read.unwrap_or(band.input * CACHE_READ),
                cache_write_5m: band.cache_write.unwrap_or(band.input * CACHE_WRITE_5M),
                // Never published, always derived.
                cache_write_1h: band.input * CACHE_WRITE_1H,
                model_id: entry
                    .map(|e| e.id.clone())
                    .unwrap_or_else(|| model.to_owned()),
            })
        }
        // The catalogue does not price this model. A gap-fill is the last word;
        // without one there is no answer, and no answer is the honest output.
        (None, None) => gap_fill.map(|local| Resolved {
            input: local.input_per_mtok,
            output: local.output_per_mtok,
            cache_read: local.input_per_mtok * CACHE_READ,
            cache_write_5m: local.input_per_mtok * CACHE_WRITE_5M,
            cache_write_1h: local.input_per_mtok * CACHE_WRITE_1H,
            model_id: local.id.to_owned(),
        }),
    }
}

fn matches(local: &LocalRate, model: &str) -> bool {
    match local.match_kind {
        Match::Exact => local.id == model,
        Match::Prefix => model.starts_with(local.id),
    }
}

/// Is this model priced at all? Used to raise a data-quality signal rather than
/// silently reporting an incomplete total.
pub fn is_known(provider: &str, model: Option<&str>) -> bool {
    let Some(model) = model else { return false };
    let catalog = crate::catalog::current();
    let priced_by_catalog = catalog
        .as_ref()
        .and_then(|catalog| catalog.resolve(provider, model))
        .is_some_and(|entry| entry.pricing.is_some());
    priced_by_catalog || LOCAL.iter().any(|local| matches(local, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-07-26T00:00:00Z";

    /// Tests run against the embedded catalogue, which is a real models.dev
    /// payload — the rates asserted below are the published ones.
    fn with_catalog() {
        if crate::catalog::current().is_none() {
            crate::catalog::install(std::sync::Arc::new(crate::catalog::embedded().unwrap()));
        }
    }

    #[test]
    fn matches_the_rates_the_provider_actually_billed() {
        with_catalog();
        // Cross-check against a real captured generation: 1 input token and 291
        // output tokens on claude-opus-5 were billed $0.000005 and $0.007275.
        let cost = price(
            "anthropic",
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
        with_catalog();
        let cost = price(
            "anthropic",
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
        with_catalog();
        let cost = price(
            "anthropic",
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
        with_catalog();
        let cost = price(
            "anthropic",
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
        with_catalog();
        // A pure cache-write call costs more than its uncached equivalent.
        let cost = price(
            "anthropic",
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
        with_catalog();
        let cost = price(
            "anthropic",
            Some("claude-opus-5-20260115"),
            NOW,
            &Tokens {
                input: Some(1_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(cost.model_id, "claude-opus-5");
        assert!((cost.input_usd - 5.00).abs() < 1e-9);
    }

    #[test]
    fn historical_rates_apply_only_inside_their_window() {
        with_catalog();
        let tokens = Tokens {
            input: Some(1_000_000),
            ..Default::default()
        };
        // claude-sonnet-5 launched at $2 and lists at $3 from September. A
        // capture from July was billed $2 however long ago it was derived.
        let during = price(
            "anthropic",
            Some("claude-sonnet-5"),
            "2026-07-26T00:00:00Z",
            &tokens,
        )
        .unwrap();
        assert!((during.input_usd - 2.00).abs() < 1e-9);
        // After the window the catalogue is the authority, whatever it says.
        let after = price(
            "anthropic",
            Some("claude-sonnet-5"),
            "2026-10-01T00:00:00Z",
            &tokens,
        )
        .unwrap();
        assert!(after.input_usd >= 2.00);
    }

    #[test]
    fn a_model_the_catalogue_never_listed_is_still_priced() {
        with_catalog();
        // claude-mythos-5 is absent from models.dev. Losing its price while
        // "improving" pricing would be a regression hiding inside an upgrade.
        let cost = price(
            "anthropic",
            Some("claude-mythos-5"),
            NOW,
            &Tokens {
                input: Some(1_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        assert!((cost.input_usd - 10.00).abs() < 1e-9);
        assert!(is_known("anthropic", Some("claude-mythos-5")));
    }

    #[test]
    fn codex_models_are_priced() {
        with_catalog();
        // These are the models that went unpriced for as long as the rates were
        // maintained by hand.
        for model in ["gpt-5.6-luna", "gpt-5.6-terra", "gpt-5.6-sol"] {
            let cost = price(
                "openai",
                Some(model),
                NOW,
                &Tokens {
                    input: Some(10_000),
                    output: Some(100),
                    ..Default::default()
                },
            );
            assert!(cost.is_some(), "{model} is not priced");
            assert!(cost.unwrap().total_usd > 0.0);
        }
    }

    #[test]
    fn a_long_context_call_is_priced_in_the_higher_band() {
        with_catalog();
        // gpt-5.6-luna doubles above 272k. The band is chosen by whole-prompt
        // context, so a call whose context is almost entirely cache reads still
        // lands in the upper band even though input_tokens is tiny.
        let small = price(
            "openai",
            Some("gpt-5.6-luna"),
            NOW,
            &Tokens {
                input: Some(1_000),
                cache_read: Some(100_000),
                ..Default::default()
            },
        )
        .unwrap();
        let large = price(
            "openai",
            Some("gpt-5.6-luna"),
            NOW,
            &Tokens {
                input: Some(1_000),
                cache_read: Some(400_000),
                ..Default::default()
            },
        )
        .unwrap();
        // Same 1_000 input tokens, twice the rate, because the prompt is bigger.
        assert!((large.input_usd / small.input_usd - 2.0).abs() < 1e-9);
    }

    #[test]
    fn an_unknown_model_has_no_cost_rather_than_a_zero_one() {
        with_catalog();
        let tokens = Tokens {
            input: Some(10),
            ..Default::default()
        };
        assert!(price("openai", Some("some-other-model"), NOW, &tokens).is_none());
        assert!(price("anthropic", None, NOW, &tokens).is_none());
        assert!(!is_known("openai", Some("some-other-model")));
        assert!(is_known("anthropic", Some("claude-opus-5")));
    }

    #[test]
    fn a_call_with_no_reported_usage_has_no_cost_rather_than_a_zero_one() {
        with_catalog();
        // The response was never captured. Pricing this at $0.00 would report a
        // call that certainly cost money as free.
        assert!(price("anthropic", Some("claude-opus-5"), NOW, &Tokens::default()).is_none());
        // A genuine zero is different from an absent one and must still price.
        let genuine_zero = price(
            "anthropic",
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
