//! What a token costs, so a run's ceiling can be a number of dollars.
//!
//! The audited run planned against a token count and reported a token count, and
//! neither answered the question the user was asking. Its budget read
//! `usedTokens 56,007,283` against a 25,000,000 cap — 94.7% of that traffic was
//! cache reads — and the run was called a blown budget when it had spent 17.9% of
//! its allowance. The traffic figure is a diagnostic; the bill is the number that
//! decides whether a run is worth continuing.
//!
//! Two rules hold this module together.
//!
//! **Money is an integer.** Millionths of a dollar, here called `µusd`. A cached
//! run costs fractions of a cent, and `f64` dollars drift when a hundred turns are
//! summed.
//!
//! **An unknown model has no price, and says so.** There is no fallback rate: a
//! guessed price is worse than a missing one, because it is missing and looks
//! answered. [`price`] returns `None` for a model this table does not know, and
//! the status reports the money as absent along with the names it could not price.
//!
//! The table is data, not law. It is versioned ([`RATES_VERSION`]), the status
//! payload carries that version, and a price change is an edit here and a new
//! version — never a change to the arithmetic.

/// The table's version. Bump it whenever a number below changes, so a reported
/// spend can be traced to the rates it was computed with.
pub const RATES_VERSION: u32 = 2;

/// Published list prices, in millionths of a dollar per million tokens.
///
/// `input` is uncached input, `cache_read` is a cache hit, `cache_write` is a
/// cache write, `output` is generated tokens. Providers differ in whether they
/// bill cache writes at all; a provider that does not simply has `cache_write`
/// equal to `input`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
}

/// One row of the table: a model prefix and its price.
#[derive(Debug, Clone, Copy)]
pub struct ModelRate {
    /// Matched as a prefix of the model id, so `mistral-large-2411` and
    /// `mistral-large-latest` both resolve to the `mistral-large` row.
    pub model: &'static str,
    pub rate: Rate,
}

const fn rate(input: u64, cache_read: u64, cache_write: u64, output: u64) -> Rate {
    Rate {
        input,
        cache_read,
        cache_write,
        output,
    }
}

/// The published list prices this build knows, in µusd per million tokens.
///
/// Prefix order matters: the longest match wins, so `gpt-5-mini` is priced as
/// itself and not as `gpt-5`.
pub const RATES: &[ModelRate] = &[
    // Mistral. Cache reads are billed at a tenth of fresh input.
    ModelRate {
        model: "mistral-large",
        rate: rate(2_000_000, 200_000, 2_500_000, 6_000_000),
    },
    ModelRate {
        model: "mistral-medium",
        rate: rate(400_000, 40_000, 500_000, 2_000_000),
    },
    ModelRate {
        model: "mistral-small",
        rate: rate(100_000, 10_000, 125_000, 300_000),
    },
    ModelRate {
        model: "magistral-medium",
        rate: rate(2_000_000, 200_000, 2_500_000, 5_000_000),
    },
    ModelRate {
        model: "magistral-small",
        rate: rate(500_000, 50_000, 625_000, 1_500_000),
    },
    ModelRate {
        model: "codestral",
        rate: rate(300_000, 30_000, 375_000, 900_000),
    },
    ModelRate {
        model: "devstral",
        rate: rate(100_000, 10_000, 125_000, 300_000),
    },
    // DeepSeek through Mistral's API: $0.30/M in, $0.007/M cached, $1.20/M out,
    // quoted 2026-11-22. Cache writes were not quoted, so they bill as input.
    ModelRate {
        model: "deepseek",
        rate: rate(300_000, 7_000, 300_000, 1_200_000),
    },
    // Anthropic. Cache writes are billed above fresh input, which is why they
    // have their own column.
    ModelRate {
        model: "claude-opus",
        rate: rate(15_000_000, 1_500_000, 18_750_000, 75_000_000),
    },
    ModelRate {
        model: "claude-sonnet",
        rate: rate(3_000_000, 300_000, 3_750_000, 15_000_000),
    },
    ModelRate {
        model: "claude-haiku",
        rate: rate(1_000_000, 100_000, 1_250_000, 5_000_000),
    },
    // OpenAI.
    ModelRate {
        model: "gpt-5-mini",
        rate: rate(250_000, 25_000, 250_000, 2_000_000),
    },
    ModelRate {
        model: "gpt-5",
        rate: rate(1_250_000, 125_000, 1_250_000, 10_000_000),
    },
];

/// The rate for a model id, or `None` when this table does not price it.
///
/// The longest matching prefix wins. A model with no row is not priced at zero:
/// zero would read as free.
pub fn for_model(model: &str) -> Option<Rate> {
    let model = model.trim().to_ascii_lowercase();
    if model.is_empty() {
        return None;
    }
    RATES
        .iter()
        .filter(|row| model.starts_with(row.model))
        .max_by_key(|row| row.model.len())
        .map(|row| row.rate)
}

/// What a member's tokens cost, in µusd.
///
/// `cached` is a subset of `input`: the providers report cache reads inside the
/// input total, which is why this subtracts rather than adds. `cache_write` is
/// not tracked by [`super::Usage`] — the agent reports reads and totals, not
/// writes — so the write column is documented here and applied by nothing. A run
/// on a provider that bills writes therefore reports a floor, not an estimate that
/// claims to be the bill.
pub fn price(model: &str, input: u64, cached: u64, output: u64) -> Option<u64> {
    let rate = for_model(model)?;
    let cached = cached.min(input);
    let fresh = input - cached;
    let micros = fresh as u128 * rate.input as u128
        + cached as u128 * rate.cache_read as u128
        + output as u128 * rate.output as u128;
    // Per million tokens, rounded up: a run that spent a fraction of a µusd has
    // still spent something, and rounding to zero makes a hundred turns free.
    Some(micros.div_ceil(1_000_000) as u64)
}

/// The models in a run that the table could not price.
///
/// Reported next to the spend, because "spend is unknown" and "spend is zero" are
/// different answers and only one of them is honest.
pub fn unpriced<'a, I>(models: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut out: Vec<String> = Vec::new();
    for model in models {
        if for_model(model).is_none() && !model.trim().is_empty() {
            let model = model.to_string();
            if !out.contains(&model) {
                out.push(model);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arithmetic, on the audited run's own numbers.
    ///
    /// w-t2 reported 39,151,829 input of which 38,344,704 was cache, and 807,125
    /// real input, plus its output. Priced at the large-model rates that is a few
    /// dollars, not a budget: the run's 25M ceiling was a token figure and the
    /// traffic figure was 94.7% cache reads, which is why "56M against a 25M cap"
    /// was read as a blowout when it was 17.9%.
    #[test]
    fn a_mostly_cached_turn_costs_a_fraction_of_its_traffic() {
        let spend = price("mistral-large-latest", 39_151_829, 38_344_704, 200_000).unwrap();
        // 807,125 fresh at $2/M  = 1,614,250 µusd
        // 38,344,704 cache reads at $0.20/M = 7,668,941 µusd
        // 200,000 output at $6/M  = 1,200,000 µusd
        //                          ≈ $10.48, which is the number the token figure
        // hid: 39.2M tokens of traffic, and the bill is ten dollars, not two.
        assert!(
            (10_000_000..=11_000_000).contains(&spend),
            "expected about ten dollars, got {spend} µusd"
        );
    }

    /// The row the rehearsal needed: before it existed, every run on this machine
    /// reported its money as unknown.
    #[test]
    fn deepseek_is_priced_at_the_quoted_rate() {
        let row = for_model("deepseek").expect("the rehearsal's model is priced");
        assert_eq!(row.input, 300_000, "$0.30 per million fresh input");
        assert_eq!(row.cache_read, 7_000, "$0.007 per million cached");
        assert_eq!(row.output, 1_200_000, "$1.20 per million output");
        assert_eq!(
            row.cache_write, row.input,
            "the cache-write column was not quoted, so it bills as input"
        );
        assert_eq!(price("deepseek", 1_000_000, 0, 1_000_000), Some(1_500_000));
        assert_eq!(for_model("deepseek-v4.1-flash"), Some(row), "dated ids resolve");
    }

    /// A model the table does not price has no price, and zero would be a lie.
    #[test]
    fn an_unknown_model_is_unpriced_rather_than_free() {
        assert_eq!(price("some-local-gguf", 1_000_000, 0, 1_000), None);
        assert_eq!(for_model(""), None);
        assert_eq!(unpriced(["some-local-gguf", "mistral-small"]), vec!["some-local-gguf"]);
    }

    /// Prefixes resolve to the longest row, so a mini is not priced as the full
    /// model and a dated variant is priced as its family.
    #[test]
    fn the_longest_prefix_wins() {
        let mini = for_model("gpt-5-mini-2025-08-07").unwrap();
        let full = for_model("gpt-5").unwrap();
        assert!(mini.output < full.output, "{mini:?} {full:?}");
        assert_eq!(
            for_model("mistral-large-2411").unwrap(),
            for_model("mistral-large").unwrap()
        );
    }

    /// The cache read is priced below fresh input, and a cache hit on a big
    /// context is what makes a long run affordable.
    #[test]
    fn a_cache_read_is_cheaper_than_fresh_input() {
        let row = for_model("claude-sonnet-4-5").unwrap();
        assert!(row.cache_read < row.input);
        assert!(row.cache_write > row.input, "a write is billed above fresh");
        let fresh = price("claude-sonnet-4-5", 1_000_000, 0, 0).unwrap();
        let cached = price("claude-sonnet-4-5", 1_000_000, 1_000_000, 0).unwrap();
        assert!(cached * 5 < fresh, "{cached} {fresh}");
    }

    /// Cached tokens are a subset of input, and a provider that reports more
    /// cached than input must not produce a negative charge.
    #[test]
    fn cached_tokens_never_exceed_the_input_they_are_part_of() {
        let spend = price("mistral-small", 1_000, 5_000, 0).unwrap();
        let all_cached = price("mistral-small", 1_000, 1_000, 0).unwrap();
        assert_eq!(spend, all_cached);
    }
}
