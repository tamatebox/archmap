//! Lines of a text by byte offset, for evidence that a parser gives as
//! spans (the declarations of a TOML manifest).

/// Where each line of a text starts, to find the line of an offset with a
/// binary search rather than by counting from the start each time.
pub(crate) struct Lines {
    starts: Vec<usize>,
}

impl Lines {
    pub(crate) fn new(text: &str) -> Self {
        let breaks = text.match_indices('\n').map(|(at, _)| at + 1);
        Lines {
            starts: std::iter::once(0).chain(breaks).collect(),
        }
    }

    /// The line, from 1, that byte `offset` is on.
    pub(crate) fn of(&self, offset: usize) -> u32 {
        self.starts.partition_point(|&start| start <= offset) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_fall_on_their_lines() {
        let lines = Lines::new("a = 1\nb = 2\n\nc = 3");
        assert_eq!(lines.of(0), 1);
        assert_eq!(lines.of(5), 1);
        assert_eq!(lines.of(6), 2);
        assert_eq!(lines.of(12), 3);
        assert_eq!(lines.of(13), 4);
        assert_eq!(lines.of(99), 4);
    }
}
