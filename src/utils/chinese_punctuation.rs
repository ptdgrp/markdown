use std::borrow::Cow;

/// Chinese 标点符号规范化处理。
///
/// 基于 chinese-copywriting-guidelines 的标点规则：
/// - 中文语境中使用全角标点
/// - 不重复使用标点符号（`！！！` → `！`）
/// - 全角标点与其他字符之间不加空格（已在 cjk.rs 中处理）
pub fn normalize_chi_punctuation(text: &str) -> Cow<'_, str> {
    if !needs_normalization(text) {
        return Cow::Borrowed(text);
    }

    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    // 使用"最近见过 CJK 字符"作为上下文判断，不被中间的 ASCII 内容重置
    // 只有遇到句末标点（。！？）后才重置
    let mut in_cjk_context = false;

    while let Some(ch) = chars.next() {
        if is_cjk_ideograph_only(ch) {
            in_cjk_context = true;
            result.push(ch);
            continue;
        }

        // 去除重复的全角标点
        if is_fullwidth_punct(ch) {
            result.push(ch);
            while chars.peek() == Some(&ch) {
                chars.next();
            }
            // 句末标点后重置上下文
            if is_sentence_end_fullwidth(ch) {
                in_cjk_context = false;
            }
            continue;
        }

        // 保留数字之间的小数点和分隔符。
        if matches!(ch, ',' | '.')
            && result.as_bytes().last().is_some_and(u8::is_ascii_digit)
            && chars.peek().is_some_and(char::is_ascii_digit)
        {
            result.push(ch);
            continue;
        }

        // 在 CJK 上下文中，将半角标点替换为全角
        if in_cjk_context {
            if let Some(fullwidth) = halfwidth_to_fullwidth(ch) {
                result.push(fullwidth);
                // 去除重复：连续相同半角标点只转换一次
                while chars.peek() == Some(&ch) {
                    chars.next();
                }
                // 也消费后续等价的全角标点
                while chars.peek().map_or(false, |&next| {
                    halfwidth_to_fullwidth(next) == Some(fullwidth) || next == fullwidth
                }) {
                    chars.next();
                }
                if is_sentence_end_fullwidth(fullwidth)
                    && !chars.peek().map_or(false, |&c| {
                        is_convertible_halfwidth_punct(c) || is_fullwidth_punct(c)
                    })
                {
                    in_cjk_context = false;
                }
                continue;
            }
        }

        result.push(ch);
    }

    Cow::Owned(result)
}

/// 仅判断 CJK 表意文字（不含标点符号），用于上下文检测
#[inline]
fn is_cjk_ideograph_only(ch: char) -> bool {
    matches!(ch,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{2A6DF}'
        | '\u{2A700}'..='\u{2B73F}'
        | '\u{2B740}'..='\u{2B81F}'
        | '\u{2B820}'..='\u{2CEAF}'
        | '\u{2CEB0}'..='\u{2EBEF}'
        | '\u{30000}'..='\u{3134F}'
        | '\u{3040}'..='\u{309F}'   // Hiragana
        | '\u{30A0}'..='\u{30FF}'   // Katakana
    )
}

/// 快速检查是否需要规范化
fn needs_normalization(text: &str) -> bool {
    let mut has_cjk = false;
    let mut prev_fullwidth: Option<char> = None;

    for ch in text.chars() {
        // 检查重复全角标点
        if is_fullwidth_punct(ch) {
            if prev_fullwidth == Some(ch) {
                return true;
            }
            prev_fullwidth = Some(ch);
            continue;
        }
        prev_fullwidth = None;

        if is_cjk_ideograph_only(ch) {
            has_cjk = true;
        }
        // CJK 上下文中出现可转换的半角标点
        if has_cjk && is_convertible_halfwidth_punct(ch) {
            return true;
        }
    }
    false
}

/// 半角标点 → 全角标点（仅在 CJK 上下文中使用）
#[inline]
fn halfwidth_to_fullwidth(ch: char) -> Option<char> {
    match ch {
        ',' => Some('\u{FF0C}'), // ，
        '.' => Some('\u{3002}'), // 。
        '!' => Some('\u{FF01}'), // ！
        '?' => Some('\u{FF1F}'), // ？
        ':' => Some('\u{FF1A}'), // ：
        ';' => Some('\u{FF1B}'), // ；
        '(' => Some('\u{FF08}'), // （
        ')' => Some('\u{FF09}'), // ）
        _ => None,
    }
}

#[inline]
fn is_convertible_halfwidth_punct(ch: char) -> bool {
    matches!(ch, ',' | '.' | '!' | '?' | ':' | ';' | '(' | ')')
}

/// 判断是否为全角标点
#[inline]
fn is_fullwidth_punct(ch: char) -> bool {
    matches!(
        ch,
        '\u{FF0C}' // ，
        | '\u{3002}' // 。
        | '\u{FF01}' // ！
        | '\u{FF1F}' // ？
        | '\u{FF1A}' // ：
        | '\u{FF1B}' // ；
        | '\u{FF08}' // （
        | '\u{FF09}' // ）
        | '\u{3001}' // 、
        | '\u{300A}' // 《
        | '\u{300B}' // 》
        | '\u{300C}' // 「
        | '\u{300D}' // 」
        | '\u{300E}' // 『
        | '\u{300F}' // 』
    )
}

/// 句末全角标点
#[inline]
fn is_sentence_end_fullwidth(ch: char) -> bool {
    matches!(ch, '\u{3002}' | '\u{FF01}' | '\u{FF1F}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halfwidth_to_fullwidth_in_chi_context() {
        assert_eq!(
            normalize_chi_punctuation("你好,世界").as_ref(),
            "你好\u{FF0C}世界"
        );
    }

    #[test]
    fn deduplicate_fullwidth_punct() {
        assert_eq!(
            normalize_chi_punctuation("德國隊竟然戰勝了巴西隊\u{FF01}\u{FF01}").as_ref(),
            "德國隊竟然戰勝了巴西隊\u{FF01}"
        );
    }

    #[test]
    fn deduplicate_halfwidth_in_chi() {
        assert_eq!(
            normalize_chi_punctuation("太好了!!!").as_ref(),
            "太好了\u{FF01}"
        );
    }

    #[test]
    fn no_change_pure_ascii() {
        let input = "Hello, world!";
        assert!(matches!(normalize_chi_punctuation(input), Cow::Borrowed(_)));
    }

    #[test]
    fn no_change_already_correct() {
        let input = "你好\u{FF0C}世界\u{FF01}";
        assert!(matches!(normalize_chi_punctuation(input), Cow::Borrowed(_)));
    }

    #[test]
    fn parentheses() {
        // CJK 上下文中的括号对，即使中间有 ASCII 也应转全角
        assert_eq!(
            normalize_chi_punctuation("核磁共振成像(NMRI)是什麼").as_ref(),
            "核磁共振成像\u{FF08}NMRI\u{FF09}是什麼"
        );
    }

    #[test]
    fn question_mark_dedup() {
        assert_eq!(
            normalize_chi_punctuation("她竟然對你說「喵」??!!").as_ref(),
            "她竟然對你說「喵」\u{FF1F}\u{FF01}"
        );
    }

    #[test]
    fn preserve_numeric_separators_in_chi_context() {
        for (input, expected) in [
            ("金额28,245.51元,已支付.", "金额28,245.51元，已支付。"),
            ("金额1,234,567.89.", "金额1,234,567.89。"),
            ("数值-0.51和1.234,56!", "数值-0.51和1.234,56！"),
            ("数量28,下一项245.结束", "数量28，下一项245。结束"),
            ("中文,245和.51", "中文，245和。51"),
            ("28,245.51", "28,245.51"),
        ] {
            assert_eq!(
                normalize_chi_punctuation(input).as_ref(),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn empty_string() {
        assert!(matches!(normalize_chi_punctuation(""), Cow::Borrowed(_)));
    }
}
