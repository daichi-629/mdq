pub fn search_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut cjk = String::new();

    let flush_word = |word: &mut String, tokens: &mut Vec<String>| {
        if !word.is_empty() {
            tokens.push(std::mem::take(word));
        }
    };
    let flush_cjk = |cjk: &mut String, tokens: &mut Vec<String>| {
        if cjk.is_empty() {
            return;
        }
        let chars: Vec<char> = cjk.chars().collect();
        if chars.len() == 1 {
            tokens.push(chars[0].to_string());
        } else {
            for pair in chars.windows(2) {
                tokens.push(pair.iter().collect());
            }
        }
        cjk.clear();
    };

    for character in text.chars() {
        if is_cjk(character) {
            flush_word(&mut word, &mut tokens);
            cjk.push(character);
        } else if character.is_alphanumeric() || character == '_' {
            flush_cjk(&mut cjk, &mut tokens);
            for lowercase in character.to_lowercase() {
                word.push(lowercase);
            }
        } else {
            flush_word(&mut word, &mut tokens);
            flush_cjk(&mut cjk, &mut tokens);
        }
    }
    flush_word(&mut word, &mut tokens);
    flush_cjk(&mut cjk, &mut tokens);
    tokens
}

pub fn index_text(text: &str) -> String {
    search_tokens(text).join(" ")
}

pub fn fts_query(text: &str) -> String {
    search_tokens(text)
        .into_iter()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3040..=0x30ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xf900..=0xfaff
            | 0xac00..=0xd7af
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_japanese_as_bigrams() {
        assert_eq!(search_tokens("格子暗号"), vec!["格子", "子暗", "暗号"]);
    }

    #[test]
    fn tokenizes_latin_words_case_insensitively() {
        assert_eq!(search_tokens("Rust CLI"), vec!["rust", "cli"]);
    }
}
