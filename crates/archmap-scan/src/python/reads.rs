//! What a Python file reads through a name one of its imports binds: the
//! attributes it takes from a module (`charge.pay`, `shop.billing.pay`),
//! and the places it uses the name in another way or mentions it in a
//! string, where the scan cannot tell what it takes.
//!
//! A light pass over the text that knows strings and comments, not the
//! grammar: a dotted chain of names in code is a read, the same chain
//! anywhere in a string (an f-string, an annotation, a docstring) only a
//! mention.

use std::collections::BTreeSet;

/// Where a file reads through one bound path.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Reads {
    /// The attributes read right after the path, each with its line.
    pub attributes: Vec<(String, u32)>,
    /// Lines that use the path itself in code: passed, compared, assigned.
    pub bare: Vec<u32>,
    /// Lines where the path appears inside a string.
    pub in_strings: Vec<u32>,
}

impl Reads {
    /// The names read through the path, or `None` when the file may take
    /// anything of it: it uses the path itself, mentions it in a string or
    /// reads nothing through it.
    pub(crate) fn names(&self) -> Option<BTreeSet<String>> {
        if self.attributes.is_empty() || !self.bare.is_empty() || !self.in_strings.is_empty() {
            return None;
        }
        Some(
            self.attributes
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
        )
    }
}

/// A dotted chain of names in code, with whether a `.` comes right before
/// it (`obj.path`, an attribute of something else).
struct Chain {
    text: String,
    line: u32,
    after_dot: bool,
}

/// The chains of names in `text`'s code and the contents of its strings,
/// each with its line, leaving out comments and the lines in `skip`.
fn split(text: &str, skip: &BTreeSet<u32>) -> (Vec<Chain>, Vec<(String, u32)>) {
    let chars: Vec<char> = text.chars().collect();
    let (mut chains, mut strings) = (Vec::new(), Vec::new());
    let mut line = 1u32;
    let mut i = 0;
    // the last character of code before the current one, apart from spaces
    let mut previous = ' ';
    // the string that starts next is an f-string, whose `{…}` hold code
    let mut formatted = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            let start = line;
            let triple = i + 2 < chars.len() && chars[i + 1] == c && chars[i + 2] == c;
            let width = if triple { 3 } else { 1 };
            i += width;
            let mut content = String::new();
            while i < chars.len() {
                if chars[i] == '\\' {
                    content.push(chars[i]);
                    if i + 1 < chars.len() {
                        if chars[i + 1] == '\n' {
                            line += 1;
                        }
                        content.push(chars[i + 1]);
                    }
                    i += 2;
                    continue;
                }
                let closes = chars[i] == c
                    && (!triple || i + 2 < chars.len() && chars[i + 1] == c && chars[i + 2] == c);
                if closes {
                    i += width;
                    break;
                }
                if chars[i] == '\n' {
                    // a one-quote string ends at the line's end
                    if !triple {
                        break;
                    }
                    line += 1;
                }
                content.push(chars[i]);
                i += 1;
            }
            if !skip.contains(&start) {
                match std::mem::take(&mut formatted) {
                    // its text is a string, what it formats is code
                    true => {
                        let (text, fields) = formatted_fields(&content);
                        strings.push((text, start));
                        for field in fields {
                            let (inner, quoted) = split(&field, &BTreeSet::new());
                            chains.extend(inner.into_iter().map(|c| Chain {
                                line: start + c.line - 1,
                                ..c
                            }));
                            strings.extend(quoted.into_iter().map(|(q, l)| (q, start + l - 1)));
                        }
                    }
                    false => strings.push((content, start)),
                }
            }
            formatted = false;
            previous = c;
            continue;
        }
        if c.is_ascii_digit() {
            // a number, `1.5e3` and `0x1f` included
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            previous = '0';
            continue;
        }
        if is_name_start(c) {
            let after_dot = previous == '.';
            let mut chain = String::new();
            loop {
                while i < chars.len() && is_name_char(chars[i]) {
                    chain.push(chars[i]);
                    i += 1;
                }
                let next_is_name = i + 1 < chars.len() && is_name_start(chars[i + 1]);
                if i < chars.len() && chars[i] == '.' && next_is_name {
                    chain.push('.');
                    i += 1;
                    continue;
                }
                break;
            }
            // a string's prefix (`f"..."`, `rb'...'`) is no name
            let prefix = i < chars.len()
                && (chars[i] == '"' || chars[i] == '\'')
                && chain.len() <= 2
                && chain.chars().all(|ch| "rRbBuUfF".contains(ch));
            formatted = prefix && chain.contains(['f', 'F']);
            if !prefix && !skip.contains(&line) {
                chains.push(Chain {
                    text: chain,
                    line,
                    after_dot,
                });
            }
            previous = 'a';
            continue;
        }
        if !c.is_whitespace() {
            previous = c;
        }
        i += 1;
    }
    (chains, strings)
}

/// An f-string's text, apart from what it formats, and the code of each
/// replacement field (`{charge.pay(order)!r:>10}`), the braces an escaped
/// `{{` or `}}` writes left in the text.
fn formatted_fields(content: &str) -> (String, Vec<String>) {
    let chars: Vec<char> = content.chars().collect();
    let (mut text, mut fields) = (String::new(), Vec::new());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' | '}' if chars.get(i + 1) == Some(&chars[i]) => {
                text.push(chars[i]);
                i += 2;
            }
            '{' => {
                // up to the brace that closes it, past nested brackets
                let mut depth = 0;
                let mut field = String::new();
                i += 1;
                while i < chars.len() {
                    match chars[i] {
                        '{' | '[' | '(' => depth += 1,
                        '}' if depth == 0 => break,
                        '}' | ']' | ')' => depth -= 1,
                        _ => {}
                    }
                    field.push(chars[i]);
                    i += 1;
                }
                fields.push(field);
                i += 1;
            }
            c => {
                text.push(c);
                i += 1;
            }
        }
    }
    (text, fields)
}

fn is_name_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_name_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// A file's code and strings, split once for the reads of several paths.
pub(crate) struct Code {
    chains: Vec<Chain>,
    strings: Vec<(String, u32)>,
}

impl Code {
    /// `text` apart from the lines in `skip` (the import statements).
    pub(crate) fn new(text: &str, skip: &BTreeSet<u32>) -> Self {
        let (chains, strings) = split(text, skip);
        Code { chains, strings }
    }

    /// What the code reads through `path`, a dotted name an import binds
    /// (`charge`, `shop.billing`, an `as` name).
    pub(crate) fn reads(&self, path: &str) -> Reads {
        reads_in(&self.chains, &self.strings, path)
    }
}

/// What `text` reads through `path`, apart from the lines in `skip`.
#[cfg(test)]
pub(crate) fn reads(text: &str, path: &str, skip: &BTreeSet<u32>) -> Reads {
    Code::new(text, skip).reads(path)
}

fn reads_in(chains: &[Chain], strings: &[(String, u32)], path: &str) -> Reads {
    let mut out = Reads::default();
    for chain in chains.iter().filter(|c| !c.after_dot) {
        let Some(rest) = chain.text.strip_prefix(path) else {
            continue;
        };
        match rest.strip_prefix('.') {
            // `m.__dict__["pay"]`, `m.__getattribute__(name)`: a way into
            // the whole module
            Some(attribute) if attribute.starts_with("__") => out.bare.push(chain.line),
            Some(attribute) => {
                let name = attribute.split('.').next().unwrap_or(attribute);
                out.attributes.push((name.to_owned(), chain.line));
            }
            None if rest.is_empty() => out.bare.push(chain.line),
            // a longer name that starts like it (`charges`)
            None => {}
        }
    }
    for (content, line) in strings {
        if mentions(content, path) {
            out.in_strings.push(*line);
        }
    }
    out
}

/// Whether `content` holds `path` as a whole name, not inside a longer one.
fn mentions(content: &str, path: &str) -> bool {
    content.match_indices(path).any(|(at, _)| {
        let before = content[..at].chars().next_back();
        let after = content[at + path.len()..].chars().next();
        !before.is_some_and(|c| is_name_char(c) || c == '.') && !after.is_some_and(is_name_char)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str, path: &str) -> Reads {
        reads(text, path, &BTreeSet::from([1]))
    }

    #[test]
    fn attributes_read_through_a_module_name_are_its_names() {
        let reads = read(
            "from shop.billing import charge\n\
             charge.pay(order)\n\
             total = charge.Rate.DAILY + other.charge.fee\n\
             charges = 1  # charge.refund in a comment\n",
            "charge",
        );
        assert_eq!(
            reads.attributes,
            [("pay".to_owned(), 2), ("Rate".to_owned(), 3)]
        );
        assert!(reads.bare.is_empty() && reads.in_strings.is_empty());
        assert_eq!(
            reads.names(),
            Some(BTreeSet::from(["Rate".to_owned(), "pay".to_owned()]))
        );
    }

    #[test]
    fn a_dotted_path_reads_what_follows_it() {
        let reads = read(
            "import shop.billing\nshop.billing.pay(o)\nshop.other()\n",
            "shop.billing",
        );
        assert_eq!(reads.attributes, [("pay".to_owned(), 2)]);
        assert_eq!(reads.names().unwrap().len(), 1);
    }

    #[test]
    fn a_name_used_itself_or_mentioned_in_a_string_may_take_anything() {
        for text in [
            "import x as m\nreload(m)\nm.f()\n",
            "import x as m\nif value is m:\n    pass\nm.f()\n",
            "import x as m\nprint(f\"m.g is {m.g()}\")\nm.f()\n",
            "import x as m\ndef h(a: \"m.Thing\"):\n    m.f()\n",
            "import x as m\n'''uses m.g\n'''\nm.f()\n",
            "import x as m\nf = m.__dict__[\"g\"]\nm.f()\n",
        ] {
            assert_eq!(read(text, "m").names(), None, "{text}");
        }
        // reading nothing is no proof of taking nothing
        assert_eq!(read("import x as m\n", "m").names(), None);
    }

    #[test]
    fn what_an_f_string_formats_is_read_as_code() {
        let reads = read(
            "import x as m\nprint(f\"{m.g()} and {m.h!r:>10} {{as is}}\")\nm.f()\n",
            "m",
        );
        assert_eq!(
            reads.attributes,
            [
                ("g".to_owned(), 2),
                ("h".to_owned(), 2),
                ("f".to_owned(), 3)
            ]
        );
        assert!(reads.in_strings.is_empty(), "{reads:?}");
        assert_eq!(reads.names().unwrap().len(), 3);
    }

    #[test]
    fn strings_numbers_and_prefixes_are_no_names() {
        let reads = read(
            "import x as m\ns = 'm.g' + r\"m\" + f'{1.5}'\nm.f()\nn = 1.5\n",
            "m",
        );
        assert_eq!(reads.attributes, [("f".to_owned(), 3)]);
        assert_eq!(reads.in_strings, [2, 2]);
    }
}
