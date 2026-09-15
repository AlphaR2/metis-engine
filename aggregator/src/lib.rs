pub struct Aggregator {
    pub venues: Vec<String>,
}

impl Aggregator {
    pub fn new() -> Self {
        Self { venues: Vec::new() }
    }
}
