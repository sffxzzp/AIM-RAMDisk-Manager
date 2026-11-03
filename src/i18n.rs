use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Chinese,
    English,
}

impl Language {
    pub fn system_default() -> Self {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
            let language_id = unsafe { GetUserDefaultUILanguage() };
            if language_id & 0x03ff == 0x0004 {
                return Self::Chinese;
            }
            return Self::English;
        }

        #[cfg(not(windows))]
        {
            let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
                .into_iter()
                .find_map(|key| std::env::var(key).ok())
                .unwrap_or_default();
            if locale.to_ascii_lowercase().starts_with("zh") {
                Self::Chinese
            } else {
                Self::English
            }
        }
    }
}

static CURRENT_LANGUAGE: AtomicU8 = AtomicU8::new(0);

pub fn set_language(language: Language) {
    let value = match language {
        Language::Chinese => 1,
        Language::English => 2,
    };
    CURRENT_LANGUAGE.store(value, Ordering::Relaxed);
}

pub fn current_language() -> Language {
    match CURRENT_LANGUAGE.load(Ordering::Relaxed) {
        1 => Language::Chinese,
        2 => Language::English,
        _ => Language::system_default(),
    }
}

pub fn text(key: &'static str) -> &'static str {
    match current_language() {
        Language::Chinese => chinese_text(key),
        Language::English => english_text(key),
    }
}

include!(concat!(env!("OUT_DIR"), "/i18n_generated.rs"));

#[derive(Clone, Debug)]
enum TemplatePart {
    Literal(String),
    Field(String),
}

#[derive(Clone, Debug)]
struct CompiledTranslation {
    chinese: Vec<TemplatePart>,
    english: Vec<TemplatePart>,
}

static COMPILED_TRANSLATIONS: OnceLock<Vec<CompiledTranslation>> = OnceLock::new();

/// Translate a previously rendered message when it matches a catalog entry.
/// This is used for saved login-mount results, whose text may outlive the
/// language that was active when the result was recorded.
pub fn translate_rendered(rendered: &str, from: Language, to: Language) -> Option<String> {
    if from == to {
        return Some(rendered.to_owned());
    }

    let translations = COMPILED_TRANSLATIONS.get_or_init(|| {
        LOCALIZED_MESSAGES
            .iter()
            .filter_map(|(_, chinese, english)| {
                Some(CompiledTranslation {
                    chinese: parse_template(chinese)?,
                    english: parse_template(english)?,
                })
            })
            .collect()
    });

    for translation in translations {
        let (source, target) = match from {
            Language::Chinese => (&translation.chinese, &translation.english),
            Language::English => (&translation.english, &translation.chinese),
        };
        let Some(values) = match_template(source, rendered) else {
            continue;
        };
        if fields_match(source, target) {
            if let Some(translated) = render_template(target, &values) {
                return Some(translated);
            }
        }
    }

    None
}

fn parse_template(template: &str) -> Option<Vec<TemplatePart>> {
    let bytes = template.as_bytes();
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut index = 0;
    let mut positional_index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'{' if bytes.get(index + 1) == Some(&b'{') => {
                literal.push('{');
                index += 2;
            }
            b'}' if bytes.get(index + 1) == Some(&b'}') => {
                literal.push('}');
                index += 2;
            }
            b'{' => {
                let end = template[index + 1..].find('}')? + index + 1;
                if !literal.is_empty() {
                    parts.push(TemplatePart::Literal(std::mem::take(&mut literal)));
                }
                let field_spec = &template[index + 1..end];
                let field_name = field_spec
                    .split([':', '!'])
                    .next()
                    .unwrap_or_default()
                    .trim();
                let field = if field_name.is_empty() {
                    let field = format!("#{positional_index}");
                    positional_index += 1;
                    field
                } else {
                    field_name.to_owned()
                };
                parts.push(TemplatePart::Field(field));
                index = end + 1;
            }
            b'}' => return None,
            _ => {
                let character = template[index..].chars().next()?;
                literal.push(character);
                index += character.len_utf8();
            }
        }
    }

    if !literal.is_empty() {
        parts.push(TemplatePart::Literal(literal));
    }
    Some(parts)
}

fn fields_match(left: &[TemplatePart], right: &[TemplatePart]) -> bool {
    let mut left_fields = left
        .iter()
        .filter_map(|part| match part {
            TemplatePart::Field(field) => Some(field.as_str()),
            TemplatePart::Literal(_) => None,
        })
        .collect::<Vec<_>>();
    let mut right_fields = right
        .iter()
        .filter_map(|part| match part {
            TemplatePart::Field(field) => Some(field.as_str()),
            TemplatePart::Literal(_) => None,
        })
        .collect::<Vec<_>>();
    left_fields.sort_unstable();
    right_fields.sort_unstable();
    left_fields == right_fields
}

fn match_template(template: &[TemplatePart], rendered: &str) -> Option<HashMap<String, String>> {
    fn visit(
        template: &[TemplatePart],
        rendered: &str,
        part_index: usize,
        offset: usize,
        values: &mut HashMap<String, String>,
    ) -> bool {
        if part_index == template.len() {
            return offset == rendered.len();
        }

        match &template[part_index] {
            TemplatePart::Literal(literal) => {
                if rendered[offset..].starts_with(literal) {
                    visit(
                        template,
                        rendered,
                        part_index + 1,
                        offset + literal.len(),
                        values,
                    )
                } else {
                    false
                }
            }
            TemplatePart::Field(field) => {
                if let Some(value) = values.get(field) {
                    if !rendered[offset..].starts_with(value) {
                        return false;
                    }
                    let value_length = value.len();
                    return visit(
                        template,
                        rendered,
                        part_index + 1,
                        offset + value_length,
                        values,
                    );
                }

                match template.get(part_index + 1) {
                    Some(TemplatePart::Literal(next_literal)) if !next_literal.is_empty() => {
                        for (relative_end, _) in rendered[offset..].match_indices(next_literal) {
                            let end = offset + relative_end;
                            values.insert(field.clone(), rendered[offset..end].to_owned());
                            if visit(template, rendered, part_index + 1, end, values) {
                                return true;
                            }
                            values.remove(field);
                        }
                        false
                    }
                    None => {
                        values.insert(field.clone(), rendered[offset..].to_owned());
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    let mut values = HashMap::new();
    visit(template, rendered, 0, 0, &mut values).then_some(values)
}

fn render_template(template: &[TemplatePart], values: &HashMap<String, String>) -> Option<String> {
    let mut rendered = String::new();
    for part in template {
        match part {
            TemplatePart::Literal(literal) => rendered.push_str(literal),
            TemplatePart::Field(field) => rendered.push_str(values.get(field)?),
        }
    }
    Some(rendered)
}
