//! Text as the PDF lays it out, read back as prose.

/// `lines` (a paragraph broken where the page broke it) as one line: lines
/// join with a space, a word hyphenated across lines rejoins, and Chinese
/// and Japanese characters (and their punctuation) join without a space.
/// Hangul keeps its spaces.
pub fn join_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for line in lines {
        let line = line.trim();
        let (Some(last), Some(first)) = (out.chars().last(), line.chars().next()) else {
            out.push_str(line);
            continue;
        };
        let before_hyphen = out.chars().rev().nth(1);
        if matches!(last, '-' | '\u{AD}')
            && before_hyphen.is_some_and(char::is_alphabetic)
            && first.is_lowercase()
        {
            out.pop();
        } else if !(tight(last) && tight(first)) && !cjk_punctuation(first) {
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

/// Characters written without spaces between them.
fn tight(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0x20000..=0x3134F // CJK Extensions B–G
    ) || cjk_punctuation(ch)
}

/// CJK and full-width punctuation, which never takes a space before it.
fn cjk_punctuation(ch: char) -> bool {
    matches!(ch as u32, 0x3000..=0x303F | 0xFF00..=0xFF65)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_join_into_prose() {
        assert_eq!(
            join_lines(["this is the end", "of the", "semantic first", "line"]),
            "this is the end of the semantic first line"
        );
        assert_eq!(join_lines(["  padded  ", "lines "]), "padded lines");
        assert_eq!(join_lines(Vec::<&str>::new()), "");
    }

    #[test]
    fn hyphenated_words_rejoin_but_dashes_and_names_stay() {
        assert_eq!(join_lines(["Klima-", "schutz"]), "Klimaschutz");
        assert_eq!(join_lines(["self-", "Attention"]), "self- Attention");
        assert_eq!(join_lines(["2020 -", "today"]), "2020 - today");
    }

    #[test]
    fn chinese_and_japanese_join_without_spaces_but_korean_keeps_them() {
        assert_eq!(
            join_lines(["注意力函数将一个查询", "和一组键值对映射"]),
            "注意力函数将一个查询和一组键值对映射"
        );
        assert_eq!(join_lines(["これは", "テストです"]), "これはテストです");
        assert_eq!(join_lines(["主要贡献", "，包括"]), "主要贡献，包括");
        assert_eq!(
            join_lines(["한국어 문장은", "띄어 씁니다"]),
            "한국어 문장은 띄어 씁니다"
        );
        assert_eq!(join_lines(["使用", "Transformer"]), "使用 Transformer");
    }
}
