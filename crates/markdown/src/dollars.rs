//! Which `$` in Markdown delimit formulas. Ported from Zeron
//! (crates/markdown/src/parser.rs, MIT, © 2026 Wing).
//!
//! pulldown-cmark's math extension pairs any two dollars it can, so prices
//! and shell text turn into formulas. [`prepare`] decides once, from a
//! math-free parse, which dollars are delimiters and escapes every other one:
//!
//! - Pandoc's rules: a single `$` opens before a non-space and closes after
//!   one, and a closer glued to a digit is a price (`$5-$10`); `$$` delimits
//!   display math; longer runs are literal. An opener without a closer in
//!   its paragraph (or cell, item, link text) stays literal.
//! - A span that reads as prose, shell, a regex or code stays text
//!   (`$5 and $10`, `$HOME/$USER`, `${A}`, `$(CC)`), as do dollars in URL
//!   words, query strings and after sigils (`?$top`, `@$ref`).
//! - Only inline text counts: dollars in code, raw HTML, autolinks, link
//!   destinations and reference labels keep their spelling, and an escaped
//!   `\$` stays literal.
//! - LaTeX's `\(…\)` and `\[…\]` become `$…$` and `$$…$$` when the body reads
//!   as TeX, not as a citation, a regex group or markdown escaping.
//!
//! Every pair is checked against the parse it produces: a block where one
//! doesn't come out as exactly one formula, or breaks a link, keeps all its
//! dollars literal.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};

/// `source` rewritten so that pulldown-cmark with `Options::ENABLE_MATH`
/// pairs only the dollars that delimit formulas: every other `$` in inline
/// text is escaped as `\$`, and `\(…\)` / `\[…\]` become `$…$` / `$$…$$`.
/// Borrowed when nothing changes. `options` are the caller's parse options.
pub(crate) fn prepare(source: &str, options: Options) -> Cow<'_, str> {
    if !source.contains(['$', '\\']) {
        return Cow::Borrowed(source);
    }
    // pulldown-cmark can panic on some malformed input; a message must never
    // take the app down with it, so such a source comes back as written.
    match catch_unwind(AssertUnwindSafe(|| rewrite(source, options))) {
        Ok(Cow::Owned(text)) if text != source => Cow::Owned(text),
        _ => Cow::Borrowed(source),
    }
}

/// The rewrite behind [`prepare`]. A block whose pairs don't survive the
/// parse is redone with its dollars literal and nothing in it rewritten (the
/// rest keep theirs); should that still fail, no dollar anywhere is math.
fn rewrite(source: &str, options: Options) -> Cow<'_, str> {
    let mut skip = Skip::default();
    for _ in 0..2 {
        // The passes run in order, each over the previous one's output;
        // `maps` walks their offsets back to `source`.
        let mut text = Cow::Borrowed(source);
        let mut maps = Vec::new();
        let mut formulas = Vec::new();
        // Links and images the source's first reading has: no formula may
        // break one.
        let mut links = Vec::new();
        if let Some(pass) = select_dollars(source, options, &skip) {
            text = Cow::Owned(pass.text);
            maps.push(pass.map);
            formulas = pass.pairs;
            links = pass.links;
        }
        if let Some(pass) =
            rewrite_math_delimiters(&text, options, &skip, |at| to_source(&maps, at..at).start)
        {
            formulas.extend(pass.pairs.into_iter().map(|pair| to_source(&maps, pair)));
            if links.is_empty() {
                links = pass
                    .links
                    .into_iter()
                    .map(|link| to_source(&maps, link))
                    .collect();
            }
            text = Cow::Owned(pass.text);
            maps.push(pass.map);
        }
        if formulas.is_empty() {
            return text;
        }
        let failed = failed_blocks(source, &text, options, &maps, &formulas, &links);
        if failed.is_empty() {
            return text;
        }
        skip.literal.extend(failed);
        // Kept sorted and disjoint for `Skip::off`'s binary search.
        skip.literal.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for range in skip.literal.drain(..) {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        skip.literal = merged;
    }
    skip.all = true;
    select_dollars(source, options, &skip)
        .map_or(Cow::Borrowed(source), |pass| Cow::Owned(pass.text))
}

/// `range` in the last pass's output, walked back through every pass to the
/// source.
fn to_source(maps: &[OffsetMap], range: Range<usize>) -> Range<usize> {
    maps.iter()
        .rev()
        .fold(range, |range, map| map.original_range(range))
}

/// The source blocks holding pairs the parse of `text` didn't keep: a pair
/// that isn't exactly one formula, or a formula that broke a link. Each
/// failure's innermost block (paragraph, heading, cell, item) — or, where no
/// block holds it (a rewrite that made its line a reference definition), its
/// line.
fn failed_blocks(
    source: &str,
    text: &str,
    options: Options,
    maps: &[OffsetMap],
    formulas: &[Range<usize>],
    links: &[Range<usize>],
) -> Vec<Range<usize>> {
    let mut math = HashSet::new();
    let mut starts = HashSet::new();
    let mut leaves = Vec::new();
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        let range = to_source(maps, range);
        match event {
            Event::InlineMath(_) | Event::DisplayMath(_) => {
                math.insert(range);
            }
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => {
                starts.insert(range.start);
            }
            Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::TableCell | Tag::Item) => {
                leaves.push(range);
            }
            _ => {}
        }
    }
    let mut failed: Vec<usize> = formulas
        .iter()
        .filter(|&pair| !math.contains(pair))
        .map(|pair| pair.start)
        .collect();
    // A formula that ate part of a link (`[a $[$] b](u)`) broke it: a link
    // the first reading had is gone, and a formula overlaps where it was.
    let mut sorted: Vec<&Range<usize>> = formulas.iter().collect();
    sorted.sort_by_key(|pair| pair.start);
    for link in links.iter().filter(|link| !starts.contains(&link.start)) {
        let i = sorted.partition_point(|pair| pair.end <= link.start);
        if sorted.get(i).is_some_and(|pair| pair.start < link.end) {
            failed.push(link.start);
        }
    }
    leaves.sort_by_key(|leaf| leaf.start);
    failed
        .into_iter()
        .map(|at| {
            // The innermost leaf holding `at`: the latest-starting one that
            // still reaches past it.
            leaves[..leaves.partition_point(|leaf| leaf.start <= at)]
                .iter()
                .rev()
                .find(|leaf| at < leaf.end)
                .cloned()
                .unwrap_or_else(|| {
                    let start = source[..at].rfind('\n').map_or(0, |nl| nl + 1);
                    let end = source[at..]
                        .find('\n')
                        .map_or(source.len(), |nl| at + nl + 1);
                    start..end
                })
        })
        .collect()
}

/// Where rewriting is off: blocks whose pairs didn't come out as the
/// formulas they were for, or (`all`) everywhere.
#[derive(Default)]
struct Skip {
    /// Source ranges (ascending, disjoint) where every dollar stays literal
    /// and nothing is rewritten.
    literal: Vec<Range<usize>>,
    all: bool,
}

impl Skip {
    /// Whether nothing at source offset `at` is rewritten.
    fn off(&self, at: usize) -> bool {
        let i = self.literal.partition_point(|range| range.end <= at);
        self.all || self.literal.get(i).is_some_and(|range| range.start <= at)
    }
}

/// A formula pass's output: the rewritten text and its offset map, each
/// kept pair's range (opener to past its closer, in the pass's input), and
/// the links and images its parse found — a formula must not break one.
struct FormulaPass {
    text: String,
    map: OffsetMap,
    pairs: Vec<Range<usize>>,
    links: Vec<Range<usize>>,
}

/// The edits a pass made, as `(position, shift)` pairs in rewritten byte
/// order: every rewritten offset past `position` maps back to the pass's
/// input by adding `shift` (the running total of bytes removed minus bytes
/// inserted up to there). Each edit replaces a delimiter with the single
/// byte at `position`, so a range starting on it maps to the original
/// delimiter's start and a range ending after it maps past its end.
#[derive(Default)]
struct OffsetMap {
    edits: Vec<(usize, isize)>,
}

impl OffsetMap {
    /// Record `delta` input bytes more (or, negative, fewer) behind the
    /// rewritten byte at `position`.
    fn shift(&mut self, position: usize, delta: isize) {
        let total = self.edits.last().map_or(0, |&(_, total)| total) + delta;
        self.edits.push((position, total));
    }

    fn original(&self, offset: usize) -> usize {
        match self.edits.partition_point(|&(at, _)| at < offset) {
            0 => offset,
            n => offset.saturating_add_signed(self.edits[n - 1].1),
        }
    }

    fn original_range(&self, range: Range<usize>) -> Range<usize> {
        self.original(range.start)..self.original(range.end)
    }
}

/// Insert a backslash before the `$` at each (ascending) offset, so it reads
/// as a literal dollar.
fn escape_dollars(text: &str, at: &[usize]) -> (String, OffsetMap) {
    let mut out = String::with_capacity(text.len() + at.len());
    let mut map = OffsetMap::default();
    let mut copied = 0;
    for &at in at {
        out.push_str(&text[copied..at]);
        map.shift(out.len(), -1);
        out.push('\\');
        copied = at;
    }
    out.push_str(&text[copied..]);
    (out, map)
}

/// One stretch of inline text pulldown-cmark reads in a source: a `Text`
/// event's range; the inline container holding it (an id that changes at
/// every block, link or raw-HTML boundary); and whether its first byte is
/// backslash-escaped. Code, raw HTML, autolinks, link destinations,
/// reference labels and definitions never produce such text, so a rewrite
/// that only touches it can't reach them.
struct InlineText {
    range: Range<usize>,
    container: u32,
    escaped: bool,
}

/// What one parse of a source says about its inline content: its text
/// stretches and its links and images.
struct Inlines {
    texts: Vec<InlineText>,
    links: Vec<Range<usize>>,
}

fn inline_texts(source: &str, options: Options) -> Inlines {
    let bytes = source.as_bytes();
    let mut texts = Vec::new();
    let mut links = Vec::new();
    let mut container = 0u32;
    // Code blocks, autolinks and reference links: text that keeps its
    // spelling.
    let mut literal = 0usize;
    let mut kept: Vec<bool> = Vec::new();
    // End of the last inline event: an escape's backslash is the one byte no
    // event covers right before the escaped character.
    let mut covered = 0usize;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        match &event {
            Event::Start(Tag::CodeBlock(_)) => {
                literal += 1;
                container += 1;
            }
            Event::End(TagEnd::CodeBlock) => {
                literal -= 1;
                container += 1;
            }
            // Autolinks spell a URL; a reference link's (or image's) text is
            // the label its definition must match, rewritten it would match
            // none.
            Event::Start(Tag::Link { link_type, .. } | Tag::Image { link_type, .. }) => {
                links.push(range.clone());
                let keep = !matches!(link_type, LinkType::Inline);
                literal += usize::from(keep);
                kept.push(keep);
                container += 1;
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                literal -= usize::from(kept.pop().unwrap_or(false));
                container += 1;
            }
            Event::Start(
                Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Superscript
                | Tag::Subscript,
            )
            | Event::End(
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Superscript
                | TagEnd::Subscript,
            ) => {}
            Event::Start(_) | Event::End(_) => container += 1,
            Event::InlineHtml(_) | Event::Html(_) => {
                container += 1;
                covered = covered.max(range.end);
            }
            Event::Text(_) => {
                // Text pulldown reports inside an earlier event (raw HTML
                // that overlaps a later block) is no candidate.
                if literal == 0 && range.start >= covered {
                    let at = range.start;
                    texts.push(InlineText {
                        range: range.clone(),
                        container,
                        escaped: at > covered && bytes[at - 1] == b'\\',
                    });
                }
                covered = covered.max(range.end);
            }
            _ => covered = covered.max(range.end),
        }
    }
    Inlines { texts, links }
}

/// Which dollars are formula delimiters, decided from a math-free parse so
/// pulldown-cmark never pairs the rest: every other `$` in inline text is
/// escaped. A run of one `$` opens before a non-space and closes after one;
/// `$$` opens and closes display math; longer runs are literal. An opener
/// pairs with the next closer of its kind in the same inline container
/// (TeX's own `*` and `_` may look like emphasis to this parse: `$x^*$ and
/// $y^*$` still pairs), unless the span reads as prose, shell or code
/// ([`dollar_prose`]) — then the opener stays a literal `$` and the closer
/// may open the next pair, as Pandoc reads `$5 and $10`. Where `skip` turns
/// rewriting off, every dollar stays literal. One parse, linear: each span
/// is checked once.
fn select_dollars(source: &str, options: Options, skip: &Skip) -> Option<FormulaPass> {
    if !source.contains('$') {
        return None;
    }
    let bytes = source.as_bytes();
    struct Run {
        start: usize,
        end: usize,
        container: u32,
    }
    let Inlines { texts, links } = inline_texts(source, options - Options::ENABLE_MATH);
    let mut runs = Vec::new();
    for text in texts {
        let mut at = text.range.start;
        // An escaped `$` (`\$`) starts its own text and stays literal.
        if text.escaped && bytes[at] == b'$' {
            at += 1;
        }
        while at < text.range.end {
            if bytes[at] != b'$' {
                at += 1;
                continue;
            }
            let start = at;
            while at < text.range.end && bytes[at] == b'$' {
                at += 1;
            }
            runs.push(Run {
                start,
                end: at,
                container: text.container,
            });
        }
    }
    if runs.is_empty() {
        return None;
    }
    let space_before = |at: usize| {
        source[..at]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace)
    };
    let space_after = |at: usize| source[at..].chars().next().is_none_or(char::is_whitespace);
    // Kept delimiter pairs: (opener, closer) byte ranges.
    let mut pairs: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    // The opener awaiting its closer, with its container.
    let mut pending: Option<(Range<usize>, u32)> = None;
    let mut words = UrlWords::default();
    for run in &runs {
        // Part of a URL or query string, or a sigil: never a delimiter.
        if words.url(bytes, run.start) {
            continue;
        }
        let mut start = run.start;
        let same = |open: &(Range<usize>, u32)| open.1 == run.container;
        // `$a$$b$`: a `$$` glued to an inline formula's content closes it
        // and opens the next.
        if run.end - start == 2
            && let Some(open) = pending.clone()
            && open.0.len() == 1
            && same(&open)
            && !space_before(start)
            && !dollar_prose(source, open.0.start..start + 1, false)
        {
            pairs.push((open.0, start..start + 1));
            pending = None;
            start += 1;
        }
        let len = run.end - start;
        if len > 2 {
            pending = None;
            continue;
        }
        if let Some(open) = pending.clone()
            && open.0.len() == len
            && same(&open)
            && (len == 2 || !space_before(start))
        {
            pending = None;
            if !dollar_prose(source, open.0.start..run.end, len == 2) {
                pairs.push((open.0, start..run.end));
                continue;
            }
        }
        let opens = len == 2 || !space_after(run.end);
        if opens && !skip.off(start) {
            pending = Some((start..run.end, run.container));
        } else if pending.as_ref().is_some_and(|open| open.1 != run.container) {
            pending = None;
        }
    }
    // Escape every dollar not in a kept pair.
    let mut kept: Vec<usize> = pairs
        .iter()
        .flat_map(|(open, close)| open.clone().chain(close.clone()))
        .collect();
    kept.sort_unstable();
    let escape: Vec<usize> = runs
        .iter()
        .flat_map(|run| run.start..run.end)
        .filter(|at| kept.binary_search(at).is_err())
        .collect();
    let (text, map) = escape_dollars(source, &escape);
    let pairs = pairs
        .into_iter()
        .map(|(open, close)| open.start..close.end)
        .collect();
    Some(FormulaPass {
        text,
        map,
        pairs,
        links,
    })
}

/// Tracks, for dollar spans in source order, whether each opens inside a URL
/// word (`scheme://…`) or right after a query or sigil character (`?$top`,
/// `&$x`, `@$ref`, `%$h` — but not TeX's `\%$`). Each word's start (past
/// leading brackets, quotes and emphasis: `**https://…`) is found once, so
/// it stays linear.
#[derive(Default)]
struct UrlWords {
    scanned: usize,
    start: usize,
}

impl UrlWords {
    fn url(&mut self, bytes: &[u8], at: usize) -> bool {
        let from = self.scanned.min(at);
        if let Some(space) = bytes[from..at].iter().rposition(u8::is_ascii_whitespace) {
            self.start = from + space + 1;
            self.start += bytes[self.start..at]
                .iter()
                .take_while(|byte| b"([<\"'*_~".contains(byte))
                .count();
        } else if self.scanned == 0 {
            self.start += bytes[self.start..at]
                .iter()
                .take_while(|byte| b"([<\"'*_~".contains(byte))
                .count();
        }
        self.scanned = self.scanned.max(at);
        let sigil = at > 0
            && matches!(bytes[at - 1], b'?' | b'&' | b'@' | b'%')
            && !(at > 1 && bytes[at - 2] == b'\\');
        let word = &bytes[self.start.min(at)..at];
        let name = word
            .iter()
            .take(32)
            .take_while(|byte| byte.is_ascii_alphanumeric() || b"+.-".contains(byte))
            .count();
        let scheme = name > 0 && word[0].is_ascii_alphabetic() && word[name..].starts_with(b"://");
        sigil || scheme
    }
}

/// Whether the dollar span `spelled` (in `source`, delimiters included)
/// reads as prose, shell or code rather than a formula ([`not_math`],
/// [`display_prose`]), judged with its neighbours: an identifier glued to
/// both sides (`TOKEN=$TOKEN -e ID=$ID`, R's `df$col`, Vue's `this.$refs`),
/// or a quoted string interpolating one (`"$name$suffix"`).
fn dollar_prose(source: &str, spelled: Range<usize>, display: bool) -> bool {
    let bytes = source.as_bytes();
    let delimiter = if display { 2 } else { 1 };
    if spelled.len() < 2 * delimiter {
        return true;
    }
    let tex = &source[spelled.start + delimiter..spelled.end - delimiter];
    let prev = spelled.start.checked_sub(1).map(|at| bytes[at]);
    let next = bytes.get(spelled.end).copied();
    if display {
        return display_prose(tex, next);
    }
    let word_after = next.is_some_and(|b| b.is_ascii_alphanumeric());
    let glued = prev.is_some_and(|b| b.is_ascii_alphanumeric() || b"=.".contains(&b));
    let quoted = prev.is_some_and(|b| b"\"'".contains(&b));
    ((glued || quoted) && word_after) || not_math(tex, next)
}

/// Whether a `$$…$$` span is prose, not display math: blank, a code span it
/// straddles (a backtick), a price (`$$5 vs $$10`: a closer glued to a
/// digit), or shell text around the PID variable (`kill $$ && echo $$`).
fn display_prose(tex: &str, next: Option<u8>) -> bool {
    // Words with no TeX syntax at all: the shell PID in a sentence (`Use $$
    // for the PID and $$ again`, `echo $$; echo $$`).
    let words = !tex.bytes().any(|b| b"\\^_{}=+<>|".contains(&b))
        && tex
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|word| word.len() >= 2);
    tex.trim().is_empty()
        || tex.contains('`')
        || next.is_some_and(|byte| byte.is_ascii_digit())
        || tex.contains("&&")
        || tex.contains("||")
        || words
}

/// Whether a `$…$` span (`tex` between the dollars, `next` the byte after
/// the closer) is really prose that happens to hold two dollar signs:
/// - Pandoc's currency rule: a closer glued to a digit is a price (`$5-$10`);
/// - a span ending in `/`, `:` or `@` joins shell variables
///   (`$HOME/$USER`, `PATH=$PATH:$HOME`, `$USER@$HOST`) — no formula ends so;
/// - an all-caps identifier whose closer runs into another identifier is a
///   shell variable (`$FOO$BAR`), while `$AB$` or `$n$th` stay math;
/// - TeX has no use for a backtick: one inside belongs to a code span the
///   dollars straddle (`$HOME and `$PATH``); nor for a `"` (`"$HOME" and
///   "$USER"`);
/// - shell and regex shapes: a trailing `,`/`;` (`array($a,$b)`), a bracket
///   closed before opened (`($foo) or ($bar)`), a braced name (`${A}${B}`),
///   an anchor (`/x$/i`, `'a$|b$'`), an amount then words (`$10k, or 5$`).
fn not_math(tex: &str, next: Option<u8>) -> bool {
    let shell_name = tex.len() > 1
        && tex
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && tex.as_bytes()[0].is_ascii_uppercase();
    // `${PREFIX}${NAME}`, `${arr[0]}`: a braced shell expansion; `$(CC)`: a
    // Make variable.
    let wrapped = |open: char, close: char, allowed: &dyn Fn(u8) -> bool| {
        tex.len() >= 3
            && tex.starts_with(open)
            && tex.ends_with(close)
            && tex[1..tex.len() - 1].bytes().all(allowed)
    };
    let braced = wrapped('{', '}', &|b| {
        b.is_ascii_alphanumeric() || b"_[].".contains(&b)
    });
    let make = wrapped('(', ')', &|b| {
        b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
    });
    // A sign raised or lowered onto what precedes it: `\mathbb{Z}^+`.
    let script_sign = {
        let mut before = tex.chars();
        before.next_back();
        before.as_str().ends_with(['^', '_', '{'])
    };
    // `($foo) or ($bar)`: a bracket closed before any opened.
    let mut depth = 0i32;
    let unbalanced = tex.bytes().any(|b| {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        depth < 0
    });
    // `$10k, or 5$`: an amount followed by words, with no TeX in it.
    let amount = tex.starts_with(|c: char| c.is_ascii_digit())
        && !tex.bytes().any(|b| b"\\^_{}=".contains(&b))
        && tex
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|word| word.len() >= 2);
    next.is_some_and(|b| b.is_ascii_digit())
        || tex.ends_with(['/', ':', '@', ',', ';'])
        || tex.contains(['`', '"'])
        || (shell_name && next.is_some_and(|b| b.is_ascii_uppercase() || b == b'_'))
        || braced
        || make
        || unbalanced
        || amount
        // Markdown, not TeX: emphasis runs (`**bold $x** y$`).
        || tex.contains("**")
        || tex.contains("__")
        // Regex anchors: `/x$/i or /y$/g`, `grep 'foo$' a && grep 'bar$'`,
        // `'a$|b$'` (a bar-led span with no space, closed by a quote — not
        // `$|x - y| < \delta$`).
        || tex.starts_with(['/', '\''])
        || (tex.starts_with('|')
            && !tex.contains(' ')
            && next.is_some_and(|b| b == b'\'' || b == b'"'))
        // Separators after a sigil (RxJS `click$, hover$`, `5$, 10$`).
        || tex.starts_with([',', ';', ':', ')'])
        // A dangling operator: `x=$a y=$b`, `$i<$n`, `$obj->$prop` — not a
        // sign in a script (`\mathbb{Z}^+`, `Na$^+$`).
        || tex.ends_with(['=', '.'])
        || (tex.ends_with(['-', '<', '>', '*', '/'])
            && !script_sign
            && next.is_some_and(|b| b.is_ascii_alphabetic()))
        || (tex.ends_with('+') && !script_sign)
}

/// Rewrite the LaTeX delimiters models favor — `\(…\)` inline, `\[…\]`
/// display — to the `$…$` / `$$…$$` pulldown-cmark understands. Only
/// delimiters pulldown itself reads as backslash escapes in inline text
/// count ([`inline_texts`]), so code, raw HTML, URLs, reference labels and
/// definitions keep their spelling wherever they sit, and `\\(` (a literal
/// backslash) is never an opener. A closer pairs with the nearest opener of
/// its kind before it in the same inline container, so an unclosed one
/// stays literal. Openers glued to a word or inside a URL or path, openers
/// where `skip` is off (`to_source` maps an offset back to the source), and
/// bodies that can't be (or survive as) TeX ([`math_body`]) stay literal.
/// Linear in the source: each body is checked once.
fn rewrite_math_delimiters(
    source: &str,
    options: Options,
    skip: &Skip,
    to_source: impl Fn(usize) -> usize,
) -> Option<FormulaPass> {
    // Without a delimiter where rewriting is on, skip the parse.
    let live = |pattern: &str| {
        source
            .match_indices(pattern)
            .any(|(at, _)| !skip.off(to_source(at)))
    };
    if !live("\\(") && !live("\\[") {
        return None;
    }
    let bytes = source.as_bytes();
    let mut words = WordScan::default();
    let mut pairs: Vec<(usize, usize, bool)> = Vec::new();
    // The latest opener of each kind (inline, display) awaiting its closer.
    let mut pending: [Option<usize>; 2] = [None, None];
    let mut container = None;
    let Inlines { texts, links } = inline_texts(source, options);
    for text in texts {
        let at = text.range.start;
        if !text.escaped || !matches!(bytes[at], b'(' | b'[' | b')' | b']') {
            continue;
        }
        if container != Some(text.container) {
            container = Some(text.container);
            pending = [None, None];
        }
        let backslash = at - 1;
        let display = matches!(bytes[at], b'[' | b']');
        let kind = usize::from(display);
        if matches!(bytes[at], b'(' | b'[') {
            if !skip.off(to_source(backslash)) && !words.glued(bytes, backslash) {
                pending[kind] = Some(backslash);
            }
        } else if let Some(open) = pending[kind].take()
            && math_body(
                &source[open + 2..backslash],
                display,
                display && mid_line(bytes, open),
            )
        {
            pairs.push((open, backslash, display));
            pending = [None, None];
        }
    }
    if pairs.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(source.len());
    let mut map = OffsetMap::default();
    let mut copied = 0;
    for &(at, close, display) in &pairs {
        out.push_str(&source[copied..at]);
        let body = &source[at + 2..close];
        if display {
            // `\[` and `$$` are the same length: no offsets move.
            out.push_str("$$");
            out.push_str(body);
            out.push_str("$$");
        } else {
            let inner = body.trim_start();
            let lead = body.len() - inner.len();
            let inner = inner.trim_end();
            let trail = body.len() - lead - inner.len();
            // Each `$` stands in for its two-byte delimiter plus the
            // whitespace it swallowed.
            map.shift(out.len(), 1 + lead as isize);
            out.push('$');
            out.push_str(inner);
            map.shift(out.len(), 1 + trail as isize);
            out.push('$');
        }
        copied = close + 2;
    }
    out.push_str(&source[copied..]);
    let pairs = pairs.iter().map(|&(at, close, _)| at..close + 2).collect();
    Some(FormulaPass {
        text: out,
        map,
        pairs,
        links,
    })
}

/// Whether a delimited body can be a formula and survives as one: not blank;
/// no unescaped `$` (it would pair with the rewritten delimiters), no
/// backtick (TeX has no use for one; it belongs to a code span the
/// delimiters straddle), no unescaped `%` (a TeX comment would swallow the
/// rest) and no URL; an inline body's surrounding whitespace on one line
/// (trimming a newline would move a line); not ending in a backslash (it
/// would escape the closing `$`); and not prose ([`prose_like`]; a display
/// body opened `mid_line` is prose unless it has TeX syntax).
fn math_body(body: &str, display: bool, mid_line: bool) -> bool {
    let inner = body.trim();
    if inner.is_empty()
        || body.contains('`')
        || body.contains("**")
        || body.contains("__")
        || body.contains("://")
        || has_unescaped(body, b'$')
        || has_unescaped(body, b'%')
    {
        return false;
    }
    if !display {
        let lead = &body[..body.len() - body.trim_start().len()];
        let trail = &body[body.trim_end().len()..];
        if lead.contains('\n') || trail.contains('\n') {
            return false;
        }
    }
    // A one-character display body is an escaped checkbox (`\[x\] Done`).
    if display && inner.chars().count() <= 1 {
        return false;
    }
    let escaping = inner
        .bytes()
        .rev()
        .take_while(|&byte| byte == b'\\')
        .count()
        % 2
        == 1;
    !escaping && !prose_like(inner, mid_line)
}

/// Whether the backslash at `at` follows other text on its line (not just
/// indentation and block markers): display math written mid-sentence is
/// usually markdown escaping (`array \[i\]`). Looks back only to the first
/// byte that isn't indentation or a marker.
fn mid_line(bytes: &[u8], at: usize) -> bool {
    bytes[..at]
        .iter()
        .rev()
        .find(|byte| !b" \t>-*+".contains(byte))
        .is_some_and(|&byte| byte != b'\n')
}

/// Whether a delimited body reads as prose, not TeX: with no TeX syntax at
/// all (a command, `^`, `_`, braces, an operator), a citation or number
/// (`\[1, 2\]`, `\(2.3\)`), words (`\(optional\)`, `\(see **note**\)`),
/// quotes or a `#`; or anything opened `mid_line` (`array \[i\]`). A short
/// variable (`\(x\)`, `\(ab\)`, `\(x, y\)`) stays math.
fn prose_like(body: &str, mid_line: bool) -> bool {
    if body.bytes().any(|byte| b"\\^_{}=+<>|&".contains(&byte)) {
        return false;
    }
    // A token mixing letters and two or more digits is a version or an
    // architecture (`x86`, `arm64`, `win32`), not a product of variables.
    let version = |word: &str| {
        word.bytes().all(|byte| byte.is_ascii_alphanumeric())
            && word.bytes().any(|byte| byte.is_ascii_alphabetic())
            && word.bytes().filter(u8::is_ascii_digit).count() >= 2
    };
    mid_line
        || body.split_whitespace().any(version)
        || body
            .bytes()
            .all(|byte| byte.is_ascii_digit() || b" .,;:-".contains(&byte))
        || body
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|word| word.len() >= 3)
        // Punctuation only (`\(.*\)`): a regex group, not a formula.
        || !body.bytes().any(|byte| byte.is_ascii_alphanumeric())
        || body.contains(['"', '#'])
        || body.contains("**")
}

/// Whether `body` holds `byte` (an ASCII punctuation char) without a
/// backslash escaping it.
fn has_unescaped(body: &str, byte: u8) -> bool {
    let mut escaped = false;
    for &b in body.as_bytes() {
        if b == byte && !escaped {
            return true;
        }
        escaped = b == b'\\' && !escaped;
    }
    false
}

/// Whether a backslash at `at` is glued to what precedes it: a word character
/// (`f\(x\)`, `x_\(1\)`), or any non-space byte inside a URL- or path-like
/// word (one holding a `/`). Asked in ascending `at` order; tracks the current
/// word incrementally, so a long line of openers stays linear.
#[derive(Default)]
struct WordScan {
    scanned: usize,
    slash: bool,
}

impl WordScan {
    fn glued(&mut self, bytes: &[u8], at: usize) -> bool {
        for &byte in &bytes[self.scanned..at] {
            if byte.is_ascii_whitespace() {
                self.slash = false;
            } else if byte == b'/' {
                self.slash = true;
            }
        }
        self.scanned = at;
        let Some(&prev) = at.checked_sub(1).and_then(|prev| bytes.get(prev)) else {
            return false;
        };
        prev.is_ascii_alphanumeric() || prev == b'_' || (self.slash && !prev.is_ascii_whitespace())
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::time::{Duration, Instant};

    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    use super::prepare;

    fn options() -> Options {
        Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_MATH
    }

    /// What a parse of `markdown` shows: its text as written, `<m>…</m>` and
    /// `<d>…</d>` around inline and display formulas (trimmed), `<c>…</c>`
    /// around code spans and a newline after each block; with `tags`, every
    /// element's start and end instead of those newlines.
    fn read(markdown: &str, options: Options, tags: bool) -> String {
        let mut out = String::new();
        for event in Parser::new_ext(markdown, options) {
            match event {
                Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                    out.push_str(&text);
                }
                Event::Code(code) => out.push_str(&format!("<c>{code}</c>")),
                Event::InlineMath(tex) => out.push_str(&format!("<m>{}</m>", tex.trim())),
                Event::DisplayMath(tex) => out.push_str(&format!("<d>{}</d>", tex.trim())),
                Event::SoftBreak => out.push(' '),
                Event::HardBreak => out.push('\n'),
                Event::Start(tag) if tags => out.push_str(&format!("<{:?}>", tag.to_end())),
                Event::End(end) if tags => out.push_str(&format!("</{end:?}>")),
                Event::End(
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::CodeBlock
                    | TagEnd::TableCell
                    | TagEnd::Item,
                ) => out.push('\n'),
                _ => {}
            }
        }
        out.trim_end_matches('\n').to_owned()
    }

    /// `source` prepared, then parsed with math.
    fn show(source: &str) -> String {
        read(&prepare(source, options()), options(), false)
    }

    fn assert_no_math(source: &str) {
        let shown = show(source);
        assert!(
            !shown.contains("<m>") && !shown.contains("<d>"),
            "{source:?} grew math: {shown}"
        );
    }

    /// The link and image destinations of `source`, prepared.
    fn links(source: &str) -> Vec<String> {
        let prepared = prepare(source, options());
        Parser::new_ext(&prepared, options())
            .filter_map(|event| match event {
                Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                    Some(dest_url.into_string())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn currency_stays_text() {
        for source in [
            "costs $5-$10 total",
            "between $5 and $10",
            "$5 and $10",
            "$x$5",
            "$5k-$10k, or 5$",
            "cost $$5 vs $$10",
        ] {
            assert_eq!(show(source), source);
        }
        assert_eq!(
            prepare("between $5 and $10", options()),
            "between \\$5 and \\$10"
        );
        // A formula followed by a non-digit is still math.
        assert_eq!(show("$5$ apples"), "<m>5</m> apples");
    }

    #[test]
    fn shell_variables_stay_text() {
        for source in [
            "Set $HOME/$USER now",
            "echo $FOO$BAR",
            "${PREFIX}${NAME}",
            "$(CC) -o $@ $^",
        ] {
            assert_eq!(show(source), source);
        }
        for source in [
            "PATH=$PATH:$HOME/bin",
            "'$USER@$HOST'",
            "${A}",
            "${a}${b}",
            "Set \"$HOME\" and \"$USER\"",
            "\"$@\" and \"$*\"",
            "($foo) or ($bar)",
            "array($a,$b)",
            "@$ref and %$ref",
            "/x$/i or /y$/g",
            "grep -E 'a$|b$'",
            "grep 'foo$' a.txt && grep 'bar$' b.txt",
            "'^v[0-9]+$' for tags and '^release-.*$'",
            "docker run -e TOKEN=$TOKEN -e ID=$ID image",
            "--user=$USER --home=$HOME",
            "DB_URL=$DB_URL, API_KEY=$API_KEY",
            "make CC=$CC CXX=$CXX",
            "x=$a y=$b",
            "use df$col and df$other",
            "this.$refs.input and call this.$emit(e)",
            "Observables like clicks$ and values$ in RxJS",
            "val s = \"$name and $age\"",
            "for $i<$n; $i++",
            "$obj->$prop",
            "kill $$ && echo $$",
            "echo $$; echo $$",
            "Use $$ for the PID and $$ again",
            "**bold $x** y$",
        ] {
            assert_no_math(source);
        }
        // Capital letters alone are still geometry, and suffixes still attach.
        assert_eq!(show("triangle $ABC$ here"), "triangle <m>ABC</m> here");
        assert_eq!(show("the $n$th one"), "the <m>n</m>th one");
    }

    #[test]
    fn inline_formulas() {
        assert_eq!(show("so $n$ holds"), "so <m>n</m> holds");
        assert_eq!(show("a $x^2$ b"), "a <m>x^2</m> b");
        assert_eq!(show("$a$$b$"), "<m>a</m><m>b</m>");
        // TeX's own `*`, `_` and `|` never read as markdown around formulas.
        assert_eq!(show("$x^*$ and $y^*$"), "<m>x^*</m> and <m>y^*</m>");
        assert_eq!(
            show("$\\bar{x}_{n}$ and $\\bar{y}_{n}$"),
            "<m>\\bar{x}_{n}</m> and <m>\\bar{y}_{n}</m>"
        );
        assert_eq!(show("$|x - y| < \\delta$"), "<m>|x - y| < \\delta</m>");
        assert!(show("_$y$_").contains("<m>y</m>"));
        assert_eq!(show("Na$^+$ ions"), "Na<m>^+</m> ions");
        assert_eq!(
            show("$n \\in \\mathbb{Z}^+$"),
            "<m>n \\in \\mathbb{Z}^+</m>"
        );
        assert_eq!(show("$(a+b)^2$"), "<m>(a+b)^2</m>");
        assert_eq!(show("$2 + 2 = 4$"), "<m>2 + 2 = 4</m>");
        assert_eq!(show("rate $50\\%$ up"), "rate <m>50\\%</m> up");
        assert_eq!(show("so $\\&$ too"), "so <m>\\&</m> too");
        // Mixed with prices, each span is judged on its own.
        assert_eq!(
            show("**$5** and $x$ then **$10**"),
            "$5 and <m>x</m> then $10"
        );
        // Nothing to change: the source comes back borrowed.
        assert!(matches!(
            prepare("so $x$ holds", options()),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            prepare("no formulas", options()),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn display_formulas() {
        assert_eq!(show("$$\\sum_i x_i$$"), "<d>\\sum_i x_i</d>");
        assert_eq!(show("$$\n\\frac{a}{b}\n$$\n"), "<d>\\frac{a}{b}</d>");
        assert_eq!(show("$$E = mc^2$$"), "<d>E = mc^2</d>");
        assert_eq!(show("$x^2$ and $$y$$"), "<m>x^2</m> and <d>y</d>");
        // Longer runs are literal.
        assert_eq!(show("$$$x$$$"), "$$$x$$$");
        // The shell's PID, beside a code span that keeps its spelling.
        assert_eq!(
            show("The PID $$ differs from `$$` in a subshell"),
            "The PID $$ differs from <c>$$</c> in a subshell"
        );
    }

    #[test]
    fn latex_delimiters_become_dollars() {
        assert_eq!(prepare("so \\(a\\) holds", options()), "so $a$ holds");
        assert_eq!(prepare("\\[b^2\\]", options()), "$$b^2$$");
        // Whitespace inside `\( … \)` is dropped (`$ x $` would not parse).
        assert_eq!(prepare("\\( x \\)", options()), "$x$");
        assert_eq!(
            show("so \\(a^2 + b^2\\) holds"),
            "so <m>a^2 + b^2</m> holds"
        );
        // A rewritten closer before a digit is not a price.
        assert_eq!(show("\\(x\\)2"), "<m>x</m>2");
        assert_eq!(show("\\[ e = mc^2 \\]"), "<d>e = mc^2</d>");
        assert_eq!(show("\\(x^*\\) and \\(y^*\\)"), "<m>x^*</m> and <m>y^*</m>");
        assert_eq!(show("k=\\(2\\pi\\)"), "k=<m>2\\pi</m>");
        // Quoted paragraphs continue over their `>` lines.
        assert_eq!(show("> q \\(x\n> y\\) z\n"), "q <m>x\ny</m> z");
        // A one-character display body is an escaped checkbox.
        assert_eq!(show("\\[b\\] done"), "[b] done");
        // `\\(` is a literal backslash then `(`.
        assert_eq!(show("a \\\\(b\\\\) c"), "a \\(b\\) c");
        for source in [
            "See \\[1\\] for details.",
            "Pandoc \\[Online\\] style.",
            "Value \\(optional\\) here.",
            "Like \\(see above\\) and \\(2.3\\).",
            "array \\[i\\]",
            "arr\\[0\\] and f\\(x\\)",
            "x_\\(1\\) and a/\\(b\\)",
            "see https://x.dev/a,\\(b\\) and /tmp/p-\\(1\\)",
            "Program Files \\(x86\\)",
            "Use \\(.*\\) to capture",
            "Escape parens as \\( and \\) in BRE",
            "\\(see **note**\\)",
            "\\(e.g. 5%\\)",
            "\\(https://x.y/z\\)",
            "<span>\\(x</span>\\)",
            "<!-- note \\(x\\) -->\n",
            "<div>\n\\(x\\)\n</div>\n",
            "> # Title \\(\n> body \\) end\n",
            "\\[x\\] Done",
        ] {
            assert_no_math(source);
        }
    }

    #[test]
    fn code_keeps_its_dollars() {
        assert_eq!(show("`$x$` and $y$"), "<c>$x$</c> and <m>y</m>");
        assert_eq!(
            show("costs $5 (`$4` with code)"),
            "costs $5 (<c>$4</c> with code)"
        );
        assert_eq!(show("`\\(x\\)` and \\(y\\)"), "<c>\\(x\\)</c> and <m>y</m>");
        assert!(show("With $HOME set, the regex `^$\\(a\\)` matches").contains("<c>^$\\(a\\)</c>"));
        for source in [
            "```\n$5 and $x$ \\(y\\)\n```\n",
            "    echo $HOME $x$\n",
            "<div>cost $5 and $x$</div>\n",
        ] {
            assert_eq!(prepare(source, options()), source);
            assert_no_math(source);
        }
        let source = "Pay $5 for `$x`.\n".repeat(40);
        assert_eq!(show(&source).matches("<c>$x</c>").count(), 40);
    }

    #[test]
    fn escaped_dollars_stay_literal() {
        assert_eq!(show("price \\$5 and $x$"), "price $5 and <m>x</m>");
        assert_eq!(show("\\$x\\$ is not math"), "$x$ is not math");
        // A lone `$` inside `\(…\)` is a literal dollar, as `\$` is.
        assert_eq!(show("\\(a $ b\\)"), "<m>a \\$ b</m>");
        assert_eq!(show("\\(a \\$ b\\)"), "<m>a \\$ b</m>");
    }

    #[test]
    fn unclosed_formulas_stay_text() {
        for source in ["so $x and more", "so $x^2 is", "so $$x", "costs $5 to $"] {
            assert_eq!(show(source), source);
        }
        assert_eq!(show("so \\(a^2"), "so (a^2");
        // A closer past the end of its block doesn't pair.
        assert_eq!(show("so $x\n\nand y$ here"), "so $x\nand y$ here");
        assert_eq!(show("\\[\nx\n\nmore \\]"), "[ x\nmore ]");
        assert_eq!(show("a \\(x\n---\ny\\)\n"), "a (x\ny)");
    }

    #[test]
    fn dollars_in_links_and_urls_stay_literal() {
        let source =
            "Call https://graph.microsoft.com/v1.0/me/messages?$select=subject&$top=10 to list";
        assert_eq!(show(source), source);
        for source in [
            "Visit https://x.dev/$a$b now",
            "**https://example.com/$id$/x**",
            "(_https://example.com/$a$b_)",
            "GET /api/items?$select=name&$expand=owner",
        ] {
            assert_no_math(source);
        }
        // Destinations keep their spelling; formulas beside the link render.
        let source = "[pay](https://x.dev/?a=$5&b=$6) then $x$";
        assert_eq!(links(source), ["https://x.dev/?a=$5&b=$6"]);
        assert_eq!(show(source), "pay then <m>x</m>");
        let source = "[from $5 to $10](u) and $y$";
        assert_eq!(links(source), ["u"]);
        assert_eq!(show(source), "from $5 to $10 and <m>y</m>");
        assert_eq!(
            show("<https://x.dev/$a$b> and $x$"),
            "https://x.dev/$a$b and <m>x</m>"
        );
        let source = "[w](https://en.wikipedia.org/wiki/Foo_\\(bar\\))";
        assert_eq!(links(source), ["https://en.wikipedia.org/wiki/Foo_(bar)"]);
        assert_no_math(source);
        // A formula never breaks the link around it.
        assert_eq!(links("[a $[$] b](u)"), ["u"]);
        // A table pipe isn't part of the URL word.
        assert!(show("|https://a.com|$x^2$|\n|-|-|\n").contains("<m>x^2</m>"));
    }

    /// Dollars that aren't math parse exactly as they do without math
    /// support: a rejected span's markup still applies.
    #[test]
    fn rejected_dollars_keep_their_markdown() {
        for source in [
            "**$5** to **$10**",
            "from $5 to [$10](u)",
            "costs $5 (`$4` with code)",
            "costs $5-$10 total, between $5 and $10",
            "$5 *a* $6 *b* $7 *c* $8",
            "pay $5, get _$10_ back and ~~$2~~",
            "PATH=$PATH:$HOME/bin and `$HOME`",
            "'$USER@$HOST' **now**",
            "> quoted $5 **b** $6\n",
            "- $1 [x](u) $2\n- $3 *i* $4\n",
            "| a | b |\n|---|---|\n| $5 **x** | $6 |\n",
            "# $5 *off* $9\n",
        ] {
            assert_eq!(
                read(&prepare(source, options()), options(), true),
                read(source, options() - Options::ENABLE_MATH, true),
                "{source:?}"
            );
        }
    }

    /// Random prose where every `$` is a price: whatever spans pulldown would
    /// pair, the result must read as a parse without math.
    #[test]
    fn random_prices_read_as_without_math() {
        const PIECES: &[&str] = &[
            "$5", "$10", "$1.50", " ", " ", "a", "b c", "**", "*", "_", "~~", "`", "``", "[",
            "](u)", "]", "(", ")", "\\*", "\n", "\n\n", "\n- ", "\n> ", "|", "é", "-", ":",
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..3000 {
            let len = 4 + (next() % 40) as usize;
            let source: String = (0..len)
                .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
                .collect();
            assert_eq!(
                read(&prepare(&source, options()), options(), true),
                read(&source, options() - Options::ENABLE_MATH, true),
                "{source:?}"
            );
        }
    }

    /// A block whose pairs fail stays literal on its own: the other blocks'
    /// formulas still render.
    #[test]
    fn a_failing_block_keeps_other_blocks_math() {
        let snippet = "$$$`$`$\\ $$$`$`$\\(\\({](\\))$$\\[]\\]{$``$\\`$) $`$$$( $/$ ";
        let shown = show(&format!("so $x^2$ here\n\n{snippet}\n\nand \\(y\\) too\n"));
        assert!(shown.starts_with("so <m>x^2</m> here\n"), "{shown}");
        assert!(shown.ends_with("and <m>y</m> too"), "{shown}");
    }

    /// Hostile shapes cost a bounded number of parses, each linear.
    #[test]
    fn hostile_input_stays_fast() {
        let snippet = "$$$`$`$\\ $$$`$`$\\(\\({](\\))$$\\[]\\]{$``$\\`$) $`$$$( $/$ ";
        let line = |unit: &str| unit.repeat((256 << 10) / unit.len());
        for (source, limit) in [
            (format!("{snippet}\n{}", "a\n".repeat(64 << 10)), 2),
            (
                format!("{}{}", "(".repeat(64 << 10), "$a".repeat(32 << 10)),
                2,
            ),
            (line("\\(x\\) *a* b "), 3),
            (line("\\( *a* "), 3),
            (line("`\\(` "), 3),
            (format!("\\(x\\) {}", "\\".repeat(256 << 10)), 3),
            (line("-\\(x\\)"), 3),
            (format!("{}$ \\)", "\\( ".repeat(85_000)), 3),
            (format!("{}$ \\]", "\\[ ".repeat(85_000)), 3),
            (line("$5 and "), 3),
            (line("$x$ "), 3),
        ] {
            let started = Instant::now();
            prepare(&source, options());
            let elapsed = started.elapsed();
            assert!(
                elapsed < Duration::from_secs(limit),
                "{elapsed:?} for {:?}…",
                &source[..24]
            );
        }
    }
}
