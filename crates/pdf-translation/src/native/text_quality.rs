//! Whether a page's text layer is too broken to translate, so Auto OCR
//! recognises the page instead. Ported from Firecrawl's pdf-inspector
//! (`src/text_quality.rs`, MIT): broken ToUnicode maps give text that is
//! printable and replacement-free but wrong, which character counts and
//! U+FFFD ratios miss.

/// English letter frequencies (percent, a–z): every Latin-script language
/// stays close to them; a shifted alphabet does not.
const ENGLISH_LETTER_FREQ: [f64; 26] = [
    8.2, 1.5, 2.8, 4.3, 12.7, 2.2, 2.0, 6.1, 7.0, 0.15, 0.8, 4.0, 2.4, 6.7, 7.5, 1.9, 0.1, 6.0,
    6.3, 9.1, 2.8, 1.0, 2.4, 0.15, 2.0, 0.07,
];

/// Whether `text`, a page's text, looks like a broken encoding: words
/// joined by `$`, runs of private-use characters, or a substitution cipher
/// of a natural language.
pub(super) fn text_looks_broken(text: &str) -> bool {
    dollar_as_space(text) || private_use_run(text) || {
        let mut stats = CipherStats::default();
        stats.add(text);
        stats.looks_garbled()
    }
}

/// `Word$Word$Word`: a CMap mapping the space to `$`.
fn dollar_as_space(text: &str) -> bool {
    let dollars = text.matches('$').count();
    if dollars <= 10 {
        return false;
    }
    let bytes = text.as_bytes();
    let between_letters = (1..bytes.len().saturating_sub(1))
        .filter(|&i| {
            bytes[i] == b'$'
                && bytes[i - 1].is_ascii_alphabetic()
                && bytes[i + 1].is_ascii_alphabetic()
        })
        .count();
    between_letters > 20 || between_letters * 2 > dollars
}

/// Glyph ids passed through as private-use code points.
fn private_use_run(text: &str) -> bool {
    let private =
        |ch: char| matches!(ch as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD);
    let (mut total, mut count, mut run, mut longest) = (0usize, 0usize, 0usize, 0usize);
    for ch in text.chars() {
        if ch.is_whitespace() {
            run = 0;
            continue;
        }
        total += 1;
        if private(ch) {
            count += 1;
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    count > 0 && (longest >= 3 || (total >= 5 && count >= 2 && count * 2 >= total))
}

/// Letter statistics of a page, for text whose every letter is shifted
/// (`Certificate` read as `8VceZWZTReV`).
#[derive(Default)]
struct CipherStats {
    letters: [u32; 26],
    ascii_letters: usize,
    vowels: usize,
    latin_extended: usize,
    non_latin: usize,
    bigrams: usize,
    case_shifts: usize,
}

impl CipherStats {
    fn add(&mut self, text: &str) {
        let mut previous: Option<char> = None;
        for ch in text.chars() {
            if ch.is_ascii_alphabetic() {
                let lower = ch.to_ascii_lowercase();
                self.letters[(lower as u8 - b'a') as usize] += 1;
                self.ascii_letters += 1;
                if matches!(lower, 'a' | 'e' | 'i' | 'o' | 'u') {
                    self.vowels += 1;
                }
                if let Some(previous) = previous {
                    self.bigrams += 1;
                    if previous.is_ascii_lowercase() && ch.is_ascii_uppercase() {
                        self.case_shifts += 1;
                    }
                }
                previous = Some(ch);
            } else {
                if ch.is_alphabetic() {
                    if matches!(ch as u32, 0xC0..=0x24F | 0x1E00..=0x1EFF) {
                        self.latin_extended += 1;
                    } else {
                        self.non_latin += 1;
                    }
                }
                previous = None;
            }
        }
    }

    /// Cosine between the letter histogram and English, as is (`sorted`
    /// false) or comparing only the shapes of the two (both sorted).
    fn english_cosine(&self, sorted: bool) -> f64 {
        if self.ascii_letters == 0 {
            return 1.0;
        }
        let n = self.ascii_letters as f64;
        let mut observed: [f64; 26] = std::array::from_fn(|i| f64::from(self.letters[i]) / n);
        let mut english = ENGLISH_LETTER_FREQ;
        if sorted {
            observed.sort_unstable_by(|a, b| b.total_cmp(a));
            english.sort_unstable_by(|a, b| b.total_cmp(a));
        }
        let dot: f64 = observed.iter().zip(english).map(|(o, e)| o * e).sum();
        let norm = |values: &[f64]| values.iter().map(|v| v * v).sum::<f64>().sqrt();
        dot / (norm(&observed) * norm(&english))
    }

    /// pdf-inspector's thresholds, checked there against 380 documents
    /// without a false positive.
    fn looks_garbled(&self) -> bool {
        if self.ascii_letters < 200 || self.non_latin > self.ascii_letters + self.latin_extended {
            return false;
        }
        if self.vowels as f64 / self.ascii_letters as f64 > 0.30 {
            return false;
        }
        let case_shifts =
            self.bigrams >= 100 && self.case_shifts as f64 >= self.bigrams as f64 * 0.10;
        let permuted = self.english_cosine(false) < 0.60 && self.english_cosine(true) >= 0.90;
        case_shifts || permuted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROSE: &str = "The attention function maps a query and a set of key value pairs to an \
        output, where the query, keys, values and output are all vectors. The output is a \
        weighted sum of the values, where the weight given to each value comes from how well \
        the query matches the corresponding key. We compute the dot products of the query with \
        all keys, divide each by the square root of the dimension, and apply a softmax.";

    /// Every letter moved along the alphabet, case kept: a shifted CMap.
    fn shifted(text: &str, by: u8) -> String {
        text.chars()
            .map(|ch| match ch {
                'a'..='z' => (b'a' + (ch as u8 - b'a' + by) % 26) as char,
                'A'..='Z' => (b'A' + (ch as u8 - b'A' + by) % 26) as char,
                _ => ch,
            })
            .collect()
    }

    #[test]
    fn real_text_in_several_scripts_is_fine() {
        assert!(!text_looks_broken(PROSE));
        assert!(!text_looks_broken(
            "注意力函数将一个查询和一组键值对映射为输出。"
        ));
        assert!(!text_looks_broken(
            "Revenue grew to $4.2m from $3.9m; costs were $1.1m."
        ));
    }

    #[test]
    fn shifted_letters_dollar_spaces_and_private_use_are_broken() {
        assert!(text_looks_broken(&shifted(PROSE, 7)));
        let dollars = PROSE.split(' ').collect::<Vec<_>>().join("$");
        assert!(text_looks_broken(&dollars));
        assert!(text_looks_broken(
            "Title \u{E001}\u{E002}\u{E003}\u{E004} more"
        ));
    }
}
