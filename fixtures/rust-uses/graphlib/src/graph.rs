pub struct Edge {
    pub from: u32,
}

impl Edge {
    pub fn new(from: u32) -> Self {
        Self { from }
    }

    pub fn weight(&self) -> u32 {
        self.from
    }

    pub fn kind(&self) -> u32 {
        Self::new(self.from).weight() + self.weight()
    }
}

pub fn build(n: u32) -> Edge {
    Edge::new(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds() {
        assert_eq!(build(1).from, 1);
    }
}

pub const LIMIT: u32 = 10;

impl Default for Edge {
    fn default() -> Self {
        Self::new(0)
    }
}

pub fn qualified() -> Edge {
    <Edge>::new(1)
}
