//! Comparison normalization between glyph Unicode (from the PDF) and the
//! expected visible text of source units.
//!
//! Both sides are reduced to the same comparison alphabet: ligatures are
//! decomposed, mathematical alphanumeric styles fold to their base letters,
//! variation selectors are dropped, typographic variants that TeX fonts
//! report inconsistently (minus/hyphen, ε/ϵ, ⋅/·, ”/") fold together, and the
//! result is NFC-composed.

use unicode_normalization::UnicodeNormalization;

/// Normalize `text` into comparison characters.
pub fn comparison_chars(text: &str) -> Vec<char> {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{FE00}'..='\u{FE0F}' | '\u{200B}'..='\u{200D}' | '\u{00A0}' | '\u{00AD}' | '\u{2060}' => {}
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            '…' => out.push_str("..."),
            c if c.is_whitespace() => {}
            c => out.push(fold(c)),
        }
    }
    let composed: String = out.nfc().collect();
    // A dotless i/j carrying an accent is the accented i/j.
    let mut result: Vec<char> = Vec::with_capacity(composed.len());
    for c in composed.chars() {
        if is_combining(c)
            && let Some(last) = result.last_mut()
            && matches!(*last, 'ı' | 'ȷ')
        {
            let base = if *last == 'ı' { 'i' } else { 'j' };
            let merged: String = [base, c].iter().collect::<String>().nfc().collect();
            result.pop();
            result.extend(merged.chars());
            continue;
        }
        result.push(c);
    }
    result
}

fn is_combining(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
}

/// Spacing accent glyphs (as reported for TeX accent glyphs) → combining mark.
pub fn spacing_accent_to_combining(c: char) -> Option<char> {
    Some(match c {
        '´' | '\u{02CA}' => '\u{301}',
        '`' | '\u{02CB}' => '\u{300}',
        'ˆ' | '^' => '\u{302}',
        '¨' => '\u{308}',
        '˜' | '~' => '\u{303}',
        '¯' | 'ˉ' => '\u{304}',
        '˙' => '\u{307}',
        '˘' => '\u{306}',
        'ˇ' => '\u{30C}',
        '˝' => '\u{30B}',
        '¸' => '\u{327}',
        '˚' => '\u{30A}',
        '˛' => '\u{328}',
        _ => return None,
    })
}

/// Fold one character to its comparison representative.
fn fold(c: char) -> char {
    let cp = c as u32;
    // Mathematical Alphanumeric Symbols: Latin letters (13 styles × 52).
    if (0x1D400..=0x1D6A3).contains(&cp) {
        let i = (cp - 0x1D400) % 52;
        return if i < 26 { char::from(b'A' + i as u8) } else { char::from(b'a' + (i - 26) as u8) };
    }
    if cp == 0x1D6A4 {
        return 'ı';
    }
    if cp == 0x1D6A5 {
        return 'ȷ';
    }
    // Greek styles (5 × 58).
    if (0x1D6A8..=0x1D7C9).contains(&cp) {
        const GREEK: &str = "ΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡϴΣΤΥΦΧΨΩ∇αβγδεζηθικλμνξοπρςστυφχψω∂ϵϑϰϕϱϖ";
        let i = ((cp - 0x1D6A8) % 58) as usize;
        return fold(GREEK.chars().nth(i).unwrap_or(c));
    }
    // Digits (5 styles × 10).
    if (0x1D7CE..=0x1D7FF).contains(&cp) {
        return char::from(b'0' + ((cp - 0x1D7CE) % 10) as u8);
    }
    match c {
        'ℎ' => 'h',
        'ℂ' | 'ℭ' => 'C',
        'ℍ' | 'ℋ' | 'ℌ' => 'H',
        'ℕ' => 'N',
        'ℙ' => 'P',
        'ℚ' => 'Q',
        'ℝ' | 'ℛ' => 'R',
        'ℤ' | 'ℨ' => 'Z',
        'ℬ' => 'B',
        'ℰ' => 'E',
        'ℱ' => 'F',
        'ℐ' => 'I',
        'ℒ' => 'L',
        'ℳ' => 'M',
        'ℯ' => 'e',
        'ℊ' => 'g',
        'ℴ' => 'o',
        '−' | '‐' | '‑' | '‒' => '-',
        '"' => '”',
        '⋅' | '∙' => '·',
        '•' => '·',
        '∶' => ':',
        'ϵ' => 'ε',
        'ϕ' => 'φ',
        'ϑ' => 'θ',
        'ϖ' => 'π',
        'ϱ' => 'ρ',
        'ς' => 'σ',
        'µ' => 'μ',
        '∣' => '|',
        '∗' => '*',
        '∼' => '~',
        '∥' => '‖',
        '〈' => '⟨',
        '〉' => '⟩',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> String {
        comparison_chars(text).into_iter().collect()
    }

    #[test]
    fn ligatures_math_alphanumerics_and_variants_fold() {
        assert_eq!(s("ﬃce"), "ffice");
        assert_eq!(s("\u{1D44E}\u{1D44F}"), "ab"); // math italic a b
        assert_eq!(s("\u{1D6FC}"), "α"); // math italic alpha
        assert_eq!(s("\u{1D7D0}"), "2"); // bold digit two
        assert_eq!(s("∑\u{FE02}"), "∑");
        assert_eq!(s("−"), "-");
        assert_eq!(s("ϵ"), s("ε"));
        assert_eq!(s("…"), "...");
        assert_eq!(s("ℝ"), "R");
    }

    #[test]
    fn composition_and_dotless_i() {
        assert_eq!(s("e\u{301}"), "é");
        assert_eq!(s("ı\u{308}"), "ï");
        assert_eq!(spacing_accent_to_combining('´'), Some('\u{301}'));
    }
}
