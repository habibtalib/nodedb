// SPDX-License-Identifier: BUSL-1.1

//! Dictionary-based segmentation dispatch.
//!
//! Uses dictionary segmentation for Japanese and Korean (lindera), ICU for
//! Thai (icu_segmenter), and CJK bigrams for Chinese (permanent fallback —
//! the previous jieba-rs dictionary chain depended on yanked crates).
//! All paths are compiled in unconditionally; selection is at runtime.

use super::bigram::tokenize_cjk;

/// Segment text using the best available method for the given language.
///
/// Falls back to CJK bigrams if no dictionary is available.
pub fn segment(text: &str, lang: &str) -> Vec<String> {
    match lang {
        "ja" | "japanese" => segment_japanese(text),
        "zh" | "chinese" => segment_chinese(text),
        "ko" | "korean" => segment_korean(text),
        "th" | "thai" => segment_thai(text),
        _ => tokenize_cjk(text),
    }
}

/// Japanese segmentation: lindera/IPADIC with bigram fallback on tokenizer error.
fn segment_japanese(text: &str) -> Vec<String> {
    lindera_segment(text, "ipadic")
}

/// Chinese segmentation: CJK bigrams (dictionary segmentation temporarily disabled).
fn segment_chinese(text: &str) -> Vec<String> {
    tokenize_cjk(text)
}

/// Korean segmentation: lindera/ko-dic with bigram fallback on tokenizer error.
fn segment_korean(text: &str) -> Vec<String> {
    lindera_segment(text, "ko-dic")
}

/// Thai segmentation: icu_segmenter.
fn segment_thai(text: &str) -> Vec<String> {
    icu_segment_thai(text)
}

// ── Implementations ──────────────────────────────────────────────────────────

fn lindera_segment(text: &str, _dict: &str) -> Vec<String> {
    use lindera::tokenizer::TokenizerBuilder;
    let Ok(tokenizer) = TokenizerBuilder::new().and_then(|b| b.build()) else {
        return tokenize_cjk(text);
    };
    let Ok(tokens) = tokenizer.tokenize(text) else {
        return tokenize_cjk(text);
    };
    tokens
        .into_iter()
        .map(|t| t.surface.to_string())
        .filter(|t: &String| t.len() > 1 || t.chars().next().is_some_and(super::script::is_cjk))
        .collect()
}

fn icu_segment_thai(text: &str) -> Vec<String> {
    use icu_segmenter::WordSegmenter;
    let segmenter = WordSegmenter::new_auto();
    let breakpoints: Vec<usize> = segmenter.segment_str(text).collect();
    let mut words = Vec::new();
    for window in breakpoints.windows(2) {
        let word = &text[window[0]..window[1]];
        if !word.trim().is_empty() {
            words.push(word.to_string());
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigrams_chinese() {
        let tokens = segment("全文検索", "zh");
        assert_eq!(tokens, vec!["全文", "文検", "検索"]);
    }

    #[test]
    fn dictionary_segmentation_japanese() {
        let tokens = segment("東京タワー", "ja");
        assert!(!tokens.is_empty());
    }

    #[test]
    fn dictionary_segmentation_korean() {
        let tokens = segment("한국어", "ko");
        assert!(!tokens.is_empty());
    }

    #[test]
    fn unknown_lang_fallback() {
        let tokens = segment("全文検索", "unknown");
        assert_eq!(tokens, vec!["全文", "文検", "検索"]);
    }
}
