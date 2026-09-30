//! How deep Oniguruma recurses compiling a pattern the way jq's `f_match`
//! does (`ONIG_SYNTAX_PERL_NG`, UTF-8) — for [`crate::compat`], which raises
//! `SIGSEGV` where that would overflow jq's C stack.
//!
//! Oniguruma compiles in two recursions over the pattern's nesting:
//!
//! * **the parser** (`prs_alts` → `prs_branch` → `prs_exp` → `prs_bag` →
//!   `prs_alts` …), one level per group of any kind, `(?:…)` and lookarounds
//!   included;
//! * **the walks over the parsed tree** (`tune_tree`, `compile_length_tree`,
//!   `compile_tree`, …), one level per node on the way down: a capturing,
//!   atomic or option group is a node, and so is a lookaround, a quantifier,
//!   an alternation and a sequence of more than one item, while `(?:…)` is
//!   none.
//!
//! [`depths`] follows the pattern as the parser does and returns both. It is
//! exact for the constructs that nest — groups of every kind, quantifiers,
//! alternations, sequences, classes — and errs on the deep side where
//! Oniguruma rewrites the tree (lookbehind, absent groups, `\X`, `\R`, case
//! folding), so that qj never survives where jq dies. It stops where
//! Oniguruma's parser stops: at a syntax error, or at jq's parse depth limit
//! (`onig_set_parse_depth_limit(1024)`); the tree is only walked when the
//! parse succeeds.

/// How deep the two recursions go for one pattern.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Depths {
    /// Groups open at once at the parser's deepest point.
    pub parse: u64,
    /// Nodes on the longest path from the root of the parsed tree to a leaf,
    /// the leaf included; 0 when the pattern doesn't parse.
    pub tree: u64,
}

/// Oniguruma's parse depth limit as jq's `main()` sets it (see
/// `crate::jq::platform::regex::PARSE_DEPTH_LIMIT`).
const PARSE_DEPTH_LIMIT: u64 = 1024;

/// A group being parsed.
struct Frame {
    /// Tree nodes the group itself adds above its body.
    node: u64,
    /// Whether whitespace and `#` comments are skipped inside (`x`).
    extended: bool,
    /// Branches finished so far, and the deepest of them.
    branches: u64,
    best: u64,
    /// The current branch: items in it, the deepest item, and the last item
    /// (which a quantifier applies to).
    items: u64,
    deepest: u64,
    last: Option<u64>,
    /// Characters in the literal string the last item is (0 if it isn't one):
    /// a literal merges with the one before it into a single string node.
    literal: u64,
}

impl Frame {
    fn new(node: u64, extended: bool) -> Frame {
        Frame {
            node,
            extended,
            branches: 0,
            best: 0,
            items: 0,
            deepest: 0,
            last: None,
            literal: 0,
        }
    }

    /// An item of `depth` nodes ends the current branch so far.
    fn item(&mut self, depth: u64) {
        self.items += 1;
        self.deepest = self.deepest.max(depth);
        self.last = Some(depth);
        self.literal = 0;
    }

    /// A literal character: part of the string before it, if there is one.
    fn literal(&mut self) {
        if self.literal == 0 {
            self.item(1);
        }
        self.literal += 1;
    }

    /// A quantifier (`*`, `{2,}`, …) on the last item, which becomes a node
    /// deeper; on a string of several characters it takes only the last one,
    /// which splits it into two items. `false` when there is nothing to
    /// quantify, which is an error.
    fn quantify(&mut self, nodes: u64) -> bool {
        let Some(last) = self.last else {
            return false;
        };
        if self.literal > 1 {
            self.items += 1;
        }
        self.literal = 0;
        self.last = Some(last + nodes);
        self.deepest = self.deepest.max(last + nodes);
        true
    }

    /// How deep the current branch is: a sequence of more than one item is a
    /// list node above them, and an empty branch is an empty node.
    fn branch_depth(&self) -> u64 {
        if self.items == 0 {
            1
        } else {
            u64::from(self.items > 1) + self.deepest
        }
    }

    /// `|`: the current branch ends and a new one starts.
    fn alternative(&mut self) {
        self.best = self.best.max(self.branch_depth());
        self.branches += 1;
        self.items = 0;
        self.deepest = 0;
        self.last = None;
        self.literal = 0;
    }

    /// How deep the whole group is, once it is closed.
    fn depth(&self) -> u64 {
        let body = self.best.max(self.branch_depth()) + u64::from(self.branches > 0);
        self.node + body
    }
}

/// Where the scan of a character class stopped.
enum Class {
    /// Past its `]`, with the class nesting it reached.
    Closed(usize, u64),
    /// Unclosed: a syntax error.
    Open,
}

/// Scans the character class whose `[` is at `start`.
fn class(p: &[u8], start: usize) -> Class {
    let mut i = start + 1;
    let mut depth = 1u64;
    let mut deepest = 1u64;
    // `]` right after `[` or `[^` is a literal.
    let mut first = true;
    while i < p.len() {
        let c = p[i];
        if c == b'^' && first && p[i - 1] == b'[' {
            i += 1;
            continue;
        }
        match c {
            b'\\' => i += 2,
            b'[' if p.get(i + 1) == Some(&b':') => {
                // A POSIX bracket, `[:alpha:]`, or a class starting with ':'.
                match p[i + 2..].windows(2).position(|w| w == b":]") {
                    Some(end) => i += end + 4,
                    None => {
                        depth += 1;
                        deepest = deepest.max(depth);
                        i += 1;
                    }
                }
            }
            b'[' => {
                depth += 1;
                deepest = deepest.max(depth);
                i += 1;
                first = true;
                continue;
            }
            b']' if !first => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Class::Closed(i, deepest);
                }
            }
            _ => i += 1,
        }
        first = false;
    }
    Class::Open
}

/// The position after a `{n}`, `{n,}`, `{,m}` or `{n,m}` interval at `i`, if
/// that is what is there (anything else is literal text).
fn interval(p: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let digits = |j: &mut usize| {
        let s = *j;
        while *j < p.len() && p[*j].is_ascii_digit() {
            *j += 1;
        }
        *j > s
    };
    let lower = digits(&mut j);
    let mut upper = false;
    if p.get(j) == Some(&b',') {
        j += 1;
        upper = digits(&mut j);
    }
    (p.get(j) == Some(&b'}') && (lower || upper)).then_some(j + 1)
}

/// The position after the name that ends at `close` (`<name>`, `'name'`),
/// starting at `i`.
fn skip_to(p: &[u8], i: usize, close: u8) -> usize {
    match p[i..].iter().position(|&c| c == close) {
        Some(n) => i + n + 1,
        None => p.len(),
    }
}

/// What an escape outside a class is: `(position after it, kind)`.
enum Escape {
    /// A character: part of a literal string.
    Char,
    /// A leaf node of its own (a character type, a backreference, an anchor).
    Leaf,
    /// A leaf Oniguruma expands into a tree this many nodes deep (`\R`, `\X`)
    /// or walks into (a subexpression call).
    Tree(u64),
    /// `\Q`: literal text up to `\E`.
    Quote,
}

fn escape(p: &[u8], i: usize) -> (usize, Escape) {
    let Some(&c) = p.get(i + 1) else {
        return (p.len(), Escape::Char);
    };
    let after = i + 2;
    match c {
        b'Q' => (after, Escape::Quote),
        b'x' | b'o' if p.get(after) == Some(&b'{') => (skip_to(p, after, b'}'), Escape::Char),
        b'x' => {
            let hex = p[after..]
                .iter()
                .take(2)
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            (after + hex, Escape::Char)
        }
        b'0' => {
            let oct = p[after..]
                .iter()
                .take(2)
                .take_while(|c| (b'0'..=b'7').contains(c))
                .count();
            (after + oct, Escape::Char)
        }
        b'1'..=b'9' => {
            let n = p[after..].iter().take_while(|c| c.is_ascii_digit()).count();
            (after + n, Escape::Leaf)
        }
        b'k' => match p.get(after) {
            Some(&b'<') => (skip_to(p, after + 1, b'>'), Escape::Leaf),
            Some(&b'\'') => (skip_to(p, after + 1, b'\''), Escape::Leaf),
            _ => (after, Escape::Char),
        },
        b'g' => match p.get(after) {
            Some(&b'<') => (skip_to(p, after + 1, b'>'), Escape::Tree(2)),
            Some(&b'\'') => (skip_to(p, after + 1, b'\''), Escape::Tree(2)),
            _ => (after, Escape::Char),
        },
        b'p' | b'P' => match p.get(after) {
            Some(&b'{') => (skip_to(p, after + 1, b'}'), Escape::Leaf),
            Some(_) => (after + 1, Escape::Leaf),
            None => (after, Escape::Leaf),
        },
        b'c' => (after + 1, Escape::Char),
        b'C' | b'M' if p.get(after) == Some(&b'-') => (after + 2, Escape::Char),
        b'X' => (after, Escape::Tree(8)),
        b'R' => (after, Escape::Tree(4)),
        b'K' | b'N' | b'O' | b'y' | b'Y' | b'A' | b'z' | b'Z' | b'G' | b'b' | b'B' | b'w'
        | b'W' | b's' | b'S' | b'd' | b'D' | b'h' | b'H' => (after, Escape::Leaf),
        _ => (after, Escape::Char),
    }
}

/// Whether `c` is whitespace that `x` (extended) mode skips.
fn space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// How deep Oniguruma's parser and its walks over the parsed tree go for
/// `pattern`, compiled with `extended` (`x`) and `ignorecase` (`i`) as jq's
/// modifiers set them.
pub fn depths(pattern: &[u8], extended: bool, ignorecase: bool) -> Depths {
    let p = pattern;
    let mut frames = vec![Frame::new(0, extended)];
    let mut parse = 0u64;
    // Oniguruma's parse depth: two a group (`prs_alts` and `prs_branch`), and
    // one a level of a character class.
    let units = |frames: &Vec<Frame>, extra: u64| 2 * frames.len() as u64 + extra;
    // Case folding can turn a literal into alternatives of strings.
    let mut folds = ignorecase;
    let mut i = 0;
    let failed = loop {
        if i >= p.len() {
            break frames.len() > 1;
        }
        let ext = frames.last().expect("the top frame").extended;
        let c = p[i];
        let top = frames.last_mut().expect("the top frame");
        if ext && space(c) {
            i += 1;
            continue;
        }
        if ext && c == b'#' {
            i = p[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(p.len(), |n| i + n + 1);
            continue;
        }
        match c {
            b'\\' => {
                let (next, kind) = escape(p, i);
                match kind {
                    Escape::Char => top.literal(),
                    Escape::Leaf => top.item(1),
                    Escape::Tree(n) => top.item(n),
                    Escape::Quote => {
                        let end = p[next..]
                            .windows(2)
                            .position(|w| w == b"\\E")
                            .map_or(p.len(), |n| next + n);
                        if end > next {
                            top.literal();
                            top.literal += (end - next - 1) as u64;
                        }
                        i = (end + 2).min(p.len());
                        continue;
                    }
                }
                i = next;
            }
            b'[' => match class(p, i) {
                Class::Closed(next, nesting) => {
                    if units(&frames, nesting) > PARSE_DEPTH_LIMIT {
                        break true;
                    }
                    frames.last_mut().expect("the top frame").item(1);
                    i = next;
                }
                Class::Open => break true,
            },
            b'*' | b'+' | b'?' => {
                if !top.quantify(1) {
                    break true;
                }
                i += 1;
                // Lazy (`*?`) is a flag; possessive (`*+`) wraps the
                // quantifier in an atomic group.
                match p.get(i) {
                    Some(b'?') => i += 1,
                    Some(b'+') => {
                        top.quantify(1);
                        i += 1;
                    }
                    _ => {}
                }
            }
            b'{' => match interval(p, i) {
                Some(next) => {
                    if !top.quantify(1) {
                        break true;
                    }
                    i = next;
                    match p.get(i) {
                        Some(b'?') => i += 1,
                        Some(b'+') => {
                            top.quantify(1);
                            i += 1;
                        }
                        _ => {}
                    }
                }
                None => {
                    top.literal();
                    i += 1;
                }
            },
            b'|' => {
                top.alternative();
                i += 1;
            }
            b')' => {
                if frames.len() == 1 {
                    // An unmatched close parenthesis.
                    break true;
                }
                let done = frames.pop().expect("an open group");
                frames
                    .last_mut()
                    .expect("the enclosing group")
                    .item(done.depth());
                i += 1;
            }
            b'(' => {
                let mut node = 1;
                let mut inner_ext = ext;
                let mut next = i + 1;
                if p.get(next) == Some(&b'*') {
                    // A callout or verb by name, `(*FAIL)`: a leaf.
                    top.item(1);
                    i = skip_to(p, next, b')');
                    continue;
                }
                if p.get(next) == Some(&b'?') {
                    next += 1;
                    match p.get(next) {
                        Some(b'#') => {
                            // A comment.
                            i = skip_to(p, next, b')');
                            continue;
                        }
                        Some(b':') => {
                            node = 0;
                            next += 1;
                        }
                        Some(b'=' | b'!' | b'>' | b'@') => next += 1,
                        Some(b'<') => match p.get(next + 1) {
                            // A lookbehind, which Oniguruma may rewrite.
                            Some(b'=' | b'!') => {
                                node = 2;
                                next += 2;
                            }
                            _ => next = skip_to(p, next + 1, b'>'),
                        },
                        Some(b'\'') => next = skip_to(p, next + 1, b'\''),
                        Some(b'P') if p.get(next + 1) == Some(&b'<') => {
                            next = skip_to(p, next + 2, b'>');
                        }
                        Some(b'~') => {
                            // An absent group: several nodes of its own.
                            node = 4;
                            next += 1;
                        }
                        Some(b'(') => {
                            // A conditional: the condition is a group of its
                            // own when it is a pattern, and a name or number
                            // otherwise.
                            node = 2;
                            if matches!(p.get(next + 1), Some(c) if c.is_ascii_digit()
                                || matches!(c, b'<' | b'\'' | b'+' | b'-'))
                            {
                                next = skip_to(p, next + 1, b')');
                            } else {
                                next += 1;
                            }
                        }
                        Some(b'{') => {
                            // A callout of contents, `(?{...})`: a leaf.
                            top.item(1);
                            i = skip_to(p, skip_to(p, next, b'}'), b')');
                            continue;
                        }
                        Some(c) if *c == b'R' || *c == b'&' || c.is_ascii_digit() => {
                            // A call, `(?R)`, `(?&name)`, `(?1)`.
                            top.item(2);
                            i = skip_to(p, next, b')');
                            continue;
                        }
                        Some(c) if c.is_ascii_alphabetic() || *c == b'-' || *c == b'^' => {
                            // Options: `(?imx-imx:...)` is a group, `(?imx)`
                            // changes the rest of the enclosing one.
                            let mut on = true;
                            let mut j = next;
                            let mut x = ext;
                            while let Some(&c) = p.get(j) {
                                match c {
                                    b'-' => on = false,
                                    b'x' => x = on,
                                    b'i' if on => folds = true,
                                    b':' | b')' => break,
                                    _ => {}
                                }
                                j += 1;
                            }
                            match p.get(j) {
                                Some(b')') => {
                                    top.extended = x;
                                    i = j + 1;
                                    continue;
                                }
                                Some(b':') => {
                                    inner_ext = x;
                                    next = j + 1;
                                }
                                _ => break true,
                            }
                        }
                        // Anything else is an error for Oniguruma; counted as
                        // a group all the same, which errs on the deep side.
                        _ => next += 1,
                    }
                }
                frames.push(Frame::new(node, inner_ext));
                // The group's frames are on the stack when the parser finds
                // the limit exceeded.
                parse = parse.max(frames.len() as u64 - 1);
                if units(&frames, 0) > PARSE_DEPTH_LIMIT {
                    break true;
                }
                i = next;
            }
            _ => {
                if c >= 0x80 {
                    // The rest of a UTF-8 sequence is the same character.
                    i += 1;
                    while i < p.len() && p[i] & 0xc0 == 0x80 {
                        i += 1;
                    }
                    top.literal();
                    continue;
                }
                match c {
                    b'.' | b'^' | b'$' => top.item(1),
                    _ => top.literal(),
                }
                i += 1;
            }
        }
    };
    let tree = if failed {
        0
    } else {
        // Case folding can expand a string into alternatives of sequences,
        // two nodes more at the bottom of the tree.
        frames[0].depth() + if folds { 2 } else { 0 }
    };
    Depths { parse, tree }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(p: &str) -> (u64, u64) {
        let r = depths(p.as_bytes(), false, false);
        (r.parse, r.tree)
    }

    fn nest(open: &str, close: &str, n: usize, mid: &str) -> String {
        format!("{}{mid}{}", open.repeat(n), close.repeat(n))
    }

    #[test]
    fn leaves_and_sequences() {
        assert_eq!(d("a"), (0, 1));
        assert_eq!(d("abc"), (0, 1));
        assert_eq!(d("a."), (0, 2));
        assert_eq!(d("a|b"), (0, 2));
        assert_eq!(d("ab*"), (0, 3));
        assert_eq!(d("a*"), (0, 2));
        assert_eq!(d(""), (0, 1));
        assert_eq!(d("[a-z]+x"), (0, 3));
    }

    /// The shapes calibrated against jq's binary (see `docs/COMPATIBILITY.md`):
    /// `n` nested groups of each kind.
    #[test]
    fn nested_groups() {
        for n in [1, 10, 40] {
            let n64 = n as u64;
            // Capturing groups around an alternation: the alternation is one
            // node more at the bottom, and the leaf another.
            assert_eq!(d(&nest("(", ")", n, "a|b")), (n64, n64 + 2), "grp{n}");
            // `(?:...)` is no node at all.
            assert_eq!(d(&nest("(?:", ")", n, "a|b")), (n64, 2), "nc{n}");
            assert_eq!(d(&nest("(?=", ")", n, "a")), (n64, n64 + 1), "la{n}");
            assert_eq!(d(&nest("(?>", ")", n, "a")), (n64, n64 + 1), "atomic{n}");
            assert_eq!(d(&nest("(?i:", ")", n, "a")), (n64, n64 + 3), "opt{n}");
            // A quantified group is two nodes a level, and so is a group in an
            // alternation, or in a sequence.
            assert_eq!(d(&nest("(", ")*", n, "a")), (n64, 2 * n64 + 1), "quant{n}");
            assert_eq!(d(&nest("(b|", ")", n, "a")), (n64, 2 * n64 + 1), "alt{n}");
            assert_eq!(
                d(&nest("(", "|b)", n, "a")),
                (n64, 2 * n64 + 1),
                "alt_right{n}"
            );
            assert_eq!(d(&nest("(a", ")", n, "b")), (n64, 2 * n64), "seq{n}");
            // A class is a leaf, however deep it nests.
            assert_eq!(d(&nest("[a", "]", n, "b")), (0, 1), "cc{n}");
        }
    }

    #[test]
    fn named_groups_and_lookbehind() {
        assert_eq!(d("(?<n>a)"), (1, 2));
        assert_eq!(d("(?'n'a)"), (1, 2));
        assert_eq!(d("(?<=a)b"), (1, 4));
        assert_eq!(d("(?<!a)"), (1, 3));
    }

    #[test]
    fn escapes_comments_and_quotes() {
        assert_eq!(d(r"\(\(\(a"), (0, 1));
        assert_eq!(d(r"[(](a)"), (1, 3));
        assert_eq!(d(r"\Q((((\E"), (0, 1));
        assert_eq!(d(r"(?#((((((((((a)"), (0, 1));
        assert_eq!(d(r"a\d"), (0, 2));
        assert_eq!(d(r"(a)\1"), (1, 3));
        // `x` skips whitespace and comments, which can hold parentheses.
        let r = depths(b"a # ((((\n b", true, false);
        assert_eq!((r.parse, r.tree), (0, 1));
        assert_eq!(d("(?x) a # ((\n"), (0, 1));
        assert_eq!(d("(?x: a # ((\n)"), (1, 2));
    }

    #[test]
    fn errors_stop_the_parse_and_skip_the_tree() {
        assert_eq!(d("((a)"), (2, 0));
        assert_eq!(d("a)"), (0, 0));
        assert_eq!(d("*a"), (0, 0));
        assert_eq!(d("[abc"), (0, 0));
        // jq's parse depth limit: two a group, so the 512th group fails.
        assert_eq!(d(&nest("(", ")", 511, "a")), (511, 512));
        assert_eq!(d(&nest("(", ")", 600, "a")), (512, 0));
    }

    #[test]
    fn quantifiers() {
        assert_eq!(d("a{2}"), (0, 2));
        assert_eq!(d("a{2,}"), (0, 2));
        assert_eq!(d("a{,2}"), (0, 2));
        assert_eq!(d("a{x}"), (0, 1));
        assert_eq!(d("a*?"), (0, 2));
        assert_eq!(d("a*+"), (0, 3));
        assert_eq!(d("(a)**"), (1, 4));
    }

    #[test]
    fn case_folding_adds_two_nodes() {
        assert_eq!(depths(b"ab", false, true).tree, 3);
        assert_eq!(d("(?i)ab"), (0, 3));
    }
}
