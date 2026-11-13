//! The words the menus and windows show, in the system's language when Snowhush speaks it, English otherwise.
//! The texts live in `locales/`, using tables inherited from Snowhush.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::simulation::Mode;

pub(crate) const LOCALES: [(&str, &str); 9] = [
    ("en", include_str!("../../locales/en.strings")),
    ("de", include_str!("../../locales/de.strings")),
    ("es", include_str!("../../locales/es.strings")),
    ("fr", include_str!("../../locales/fr.strings")),
    ("ja", include_str!("../../locales/ja.strings")),
    ("pt", include_str!("../../locales/pt.strings")),
    ("ru", include_str!("../../locales/ru.strings")),
    ("uk", include_str!("../../locales/uk.strings")),
    ("zh-Hans", include_str!("../../locales/zh-Hans.strings")),
];

static ENGLISH: OnceLock<HashMap<String, String>> = OnceLock::new();
static CHOSEN: OnceLock<(&str, HashMap<String, String>)> = OnceLock::new();

/// Speaks the first of the user's languages, most preferred first as BCP 47 or POSIX tags ("pt-BR", "ru_RU.UTF-8"),
/// that Snowhush has; called once, before anything is shown.
pub fn choose<S: AsRef<str>>(preferred: &[S]) {
    let Some((code, text)) = preferred_language(preferred) else { return };
    let _ = CHOSEN.set((code, parse(text)));
}

fn preferred_language<S: AsRef<str>>(preferred: &[S]) -> Option<(&'static str, &'static str)> {
    preferred.iter().find_map(|tag| language(tag.as_ref()))
}

/// The language spoken, as its `locales/` name: "en", "pt", "zh-Hans".
pub fn code() -> &'static str {
    CHOSEN.get().map_or("en", |(code, _)| code)
}

/// The text for `key`, falling back to English.
pub fn tr(key: &str) -> &'static str {
    let english = ENGLISH.get_or_init(|| parse(LOCALES[0].1));
    lookup(CHOSEN.get().map(|(_, chosen)| chosen), english, key)
}

fn lookup<'a>(chosen: Option<&'a HashMap<String, String>>, english: &'a HashMap<String, String>, key: &str) -> &'a str {
    chosen.and_then(|table| table.get(key)).or_else(|| english.get(key)).map_or("", String::as_str)
}

/// The text for `prefix` and what falls, in `Mode::ALL`'s order: "windows.snow.autumn".
pub fn kinds(prefix: &str, modes: &[Mode]) -> &'static str {
    let mut key = prefix.to_string();
    for mode in Mode::ALL.iter().filter(|mode| modes.contains(mode)) {
        key.push('.');
        key.push_str(mode_key(*mode));
    }
    tr(&key)
}

/// `tr(key)` with each `{name}` filled in.
pub fn fill(key: &str, values: &[(&str, &str)]) -> String {
    fill_template(tr(key), values)
}

fn fill_template(template: &str, values: &[(&str, &str)]) -> String {
    let mut result = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find('}') else { break };
        let name = &rest[1..end];
        if let Some((_, value)) = values.iter().find(|(key, _)| *key == name) {
            result.push_str(value);
        } else {
            result.push_str(&rest[..=end]);
        }
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    result
}

fn mode_key(mode: Mode) -> &'static str {
    match mode {
        Mode::Snow => "snow",
        Mode::Autumn => "autumn",
        Mode::Rain => "rain",
        Mode::Blossom => "blossom",
    }
}

/// The language for a tag, with its table. Traditional Chinese has none of its own and falls back to English rather
/// than to Simplified.
pub(crate) fn language(tag: &str) -> Option<(&'static str, &'static str)> {
    let tag = tag.split(['.', '@']).next()?.replace('_', "-").to_ascii_lowercase();
    let mut parts = tag.split('-');
    let name = match parts.next()? {
        "zh" => {
            let rest: Vec<&str> = parts.collect();
            let traditional = rest.iter().any(|part| matches!(*part, "hant" | "tw" | "hk" | "mo"));
            if traditional { return None } else { "zh-Hans" }
        }
        primary => primary,
    };
    LOCALES.iter().find(|(code, _)| *code == name).copied()
}

/// Reads an Apple `.strings` table: `"key" = "value";` lines, with `/* */` and `//` comments.
pub(crate) fn parse(text: &str) -> HashMap<String, String> {
    let mut table = HashMap::new();
    let mut chars = text.chars().peekable();
    let mut strings = Vec::new();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                let mut string = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => string.push('\n'),
                            Some('t') => string.push('\t'),
                            Some(other) => string.push(other),
                            None => {}
                        },
                        other => string.push(other),
                    }
                }
                strings.push(string);
            }
            ';' => {
                if let [key, value] = strings.as_slice() {
                    table.insert(key.clone(), value.clone());
                }
                strings.clear();
            }
            _ => {}
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(value: &str) -> BTreeSet<&str> {
        value.split('{').skip(1).filter_map(|part| part.split_once('}').map(|(name, _)| name)).collect()
    }

    #[test]
    fn tables_have_identical_keys_and_placeholders() {
        let english = parse(LOCALES[0].1);
        for (code, source) in LOCALES {
            let table = parse(source);
            assert_eq!(english.len(), table.len(), "{code}");
            for (key, original) in &english {
                let translation = table.get(key).unwrap_or_else(|| panic!("{code} missing {key}"));
                assert!(!translation.is_empty(), "{code}: {key} is empty");
                assert_eq!(placeholders(original), placeholders(translation), "{code}: {key}");
            }
        }
    }

    #[test]
    fn language_tags_and_preference_order() {
        for (tag, expected) in
            [("ru-RU", "ru"), ("uk_UA.UTF-8", "uk"), ("pt-BR", "pt"), ("zh-CN", "zh-Hans"), ("zh-Hans-SG", "zh-Hans"), ("en-GB", "en")]
        {
            assert_eq!(language(tag).map(|x| x.0), Some(expected));
        }
        for tag in ["zh-Hant", "zh-TW", "zh-HK", "it-IT", ""] {
            assert!(language(tag).is_none(), "{tag}");
        }
        let preferred = ["it-IT", "fr-FR", "de-DE"];
        assert_eq!(preferred_language(&preferred).unwrap().0, "fr");
        assert!(preferred_language(&["it-IT", "zh-TW"]).is_none());
    }

    #[test]
    fn substitutions_do_not_reinterpret_values() {
        assert_eq!(fill_template("{old} → {version}", &[("old", "{version}"), ("version", "1.2")]), "{version} → 1.2");
        assert_eq!(fill_template("{missing} {unfinished", &[]), "{missing} {unfinished");
    }

    #[test]
    fn missing_translation_uses_english() {
        let english = parse(LOCALES[0].1);
        let chosen = parse("\"snow\" = \"Schnee\";");
        assert_eq!(lookup(Some(&chosen), &english, "snow"), "Schnee");
        assert_eq!(lookup(Some(&chosen), &english, "rain"), "Rain");
        assert_eq!(lookup(None, &english, "rain"), "Rain");
        assert_eq!(lookup(Some(&chosen), &english, "unknown"), "");
    }

    #[test]
    fn every_mode_combination_has_a_caption() {
        for (code, source) in LOCALES {
            let table = parse(source);
            for key in [
                "windows.snow",
                "windows.autumn",
                "windows.rain",
                "windows.blossom",
                "windows.snow.autumn",
                "windows.snow.rain",
                "windows.snow.blossom",
                "windows.autumn.rain",
                "windows.autumn.blossom",
                "windows.rain.blossom",
                "clear.snow",
                "clear.autumn",
                "clear.blossom",
                "clear.snow.autumn",
                "clear.snow.blossom",
                "clear.autumn.blossom",
            ] {
                assert!(table.contains_key(key), "{code} missing {key}");
            }
        }
    }
}
