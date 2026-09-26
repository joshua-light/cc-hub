#[derive(Clone, Copy, Debug)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_creation_per_mtok: f64,
}

const DEFAULT_PRICING: ModelPricing = ModelPricing {
    input_per_mtok: 3.0,
    output_per_mtok: 15.0,
    cache_read_per_mtok: 0.30,
    cache_creation_per_mtok: 3.75,
};

pub(super) fn pricing_for(model: &str) -> ModelPricing {
    // Family match — strip a trailing -YYYYMMDD suffix.
    let family = strip_date_suffix(model);
    match family {
        "claude-opus-4-7" | "claude-opus-4-6" | "claude-opus-4-5" => ModelPricing {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
            cache_read_per_mtok: 0.50,
            cache_creation_per_mtok: 6.25,
        },
        "claude-sonnet-4-7" | "claude-sonnet-4-6" | "claude-sonnet-4-5" => ModelPricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.30,
            cache_creation_per_mtok: 3.75,
        },
        "claude-haiku-4-5" | "claude-haiku-4-6" => ModelPricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cache_read_per_mtok: 0.10,
            cache_creation_per_mtok: 1.25,
        },
        _ => DEFAULT_PRICING,
    }
}

pub(super) fn strip_date_suffix(model: &str) -> &str {
    let bytes = model.as_bytes();
    if bytes.len() >= 9 && bytes[bytes.len() - 9] == b'-' {
        let suffix = &bytes[bytes.len() - 8..];
        if suffix.iter().all(|b| b.is_ascii_digit()) {
            return &model[..model.len() - 9];
        }
    }
    model
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }

    pub(super) fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
    }
}

pub(super) fn cost_of(tokens: &Tokens, p: &ModelPricing) -> f64 {
    (tokens.input as f64 * p.input_per_mtok
        + tokens.output as f64 * p.output_per_mtok
        + tokens.cache_read as f64 * p.cache_read_per_mtok
        + tokens.cache_creation as f64 * p.cache_creation_per_mtok)
        / 1_000_000.0
}
