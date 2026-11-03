use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn build_adapter_shim() {
    let target = std::env::var("TARGET").expect("Cargo did not set TARGET");
    let host = std::env::var("HOST").expect("Cargo did not set HOST");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"));
    println!("cargo:rerun-if-changed=src/ramdyn_adapter_shim.rs");

    if !target.starts_with("x86_64-pc-windows-") || host != target {
        return;
    }

    let output = out_dir.join("aimapi-adapter-shim.dll");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = Command::new(rustc)
        .arg("--crate-name")
        .arg("aimapi_adapter_shim")
        .arg("--crate-type")
        .arg("cdylib")
        .arg("--edition=2021")
        .arg("--target")
        .arg(&target)
        .arg("-C")
        .arg("panic=abort")
        .arg("-C")
        .arg("opt-level=2")
        .arg("-o")
        .arg(&output)
        .arg("src/ramdyn_adapter_shim.rs")
        .status()
        .unwrap_or_else(|error| panic!("failed to compile the AIM adapter shim: {error}"));
    if !status.success() {
        panic!("failed to compile the AIM adapter shim (exit status {status})");
    }
}

#[derive(serde::Deserialize)]
struct Catalog {
    locale: String,
    messages: BTreeMap<String, String>,
}

fn rust_string(value: &str) -> String {
    format!("{value:?}")
}

fn format_fields(template: &str) -> Option<Vec<String>> {
    let bytes = template.as_bytes();
    let mut fields = Vec::new();
    let mut index = 0;
    let mut positional_index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'{' if bytes.get(index + 1) == Some(&b'{') => index += 2,
            b'}' if bytes.get(index + 1) == Some(&b'}') => index += 2,
            b'{' => {
                let end = template[index + 1..].find('}')? + index + 1;
                let field_spec = &template[index + 1..end];
                let field_name = field_spec
                    .split([':', '!'])
                    .next()
                    .unwrap_or_default()
                    .trim();
                if field_name.is_empty() {
                    fields.push(format!("#{positional_index}"));
                    positional_index += 1;
                } else {
                    fields.push(field_name.to_owned());
                }
                index = end + 1;
            }
            b'}' => return None,
            _ => {
                let character = template[index..].chars().next()?;
                index += character.len_utf8();
            }
        }
    }

    fields.sort_unstable();
    Some(fields)
}

fn read_catalog(path: &str, expected_locale: &str) -> Catalog {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("failed to read {path}: {error}"));
    let catalog: Catalog = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("invalid localization catalog {path}: {error}"));
    if catalog.locale != expected_locale {
        panic!(
            "localization catalog {path} declares locale {:?}, expected {:?}",
            catalog.locale, expected_locale
        );
    }
    catalog
}

fn main() {
    println!("cargo:rerun-if-changed=locales/en.json");
    println!("cargo:rerun-if-changed=locales/zh-CN.json");
    println!("cargo:rerun-if-changed=assets/VD.ico");
    build_adapter_shim();

    let chinese = read_catalog("locales/zh-CN.json", "zh-CN");
    let english = read_catalog("locales/en.json", "en");

    for key in chinese.messages.keys() {
        if !key.strip_prefix("msg_").is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            panic!("invalid localization key {key:?}; expected msg_<number>");
        }
        if !english.messages.contains_key(key) {
            panic!("English localization catalog is missing key {key:?}");
        }
    }
    for key in english.messages.keys() {
        if !chinese.messages.contains_key(key) {
            panic!("Chinese localization catalog is missing key {key:?}");
        }
    }
    for (key, chinese_text) in &chinese.messages {
        let english_text = &english.messages[key];
        let chinese_fields = format_fields(chinese_text)
            .unwrap_or_else(|| panic!("invalid format template in Chinese message {key:?}"));
        let english_fields = format_fields(english_text)
            .unwrap_or_else(|| panic!("invalid format template in English message {key:?}"));
        if chinese_fields != english_fields {
            panic!("format fields differ between locales for message {key:?}");
        }
    }

    let mut generated = String::new();
    generated
        .push_str("pub fn chinese_text(key: &'static str) -> &'static str {\n    match key {\n");
    for (key, value) in &chinese.messages {
        generated.push_str(&format!(
            "        {} => {},\n",
            rust_string(key),
            rust_string(value)
        ));
    }
    generated.push_str("        _ => key,\n    }\n}\n\n");

    generated
        .push_str("pub fn english_text(key: &'static str) -> &'static str {\n    match key {\n");
    for (key, value) in &english.messages {
        generated.push_str(&format!(
            "        {} => {},\n",
            rust_string(key),
            rust_string(value)
        ));
    }
    generated.push_str("        _ => key,\n    }\n}\n\n");

    generated.push_str("pub const LOCALIZED_MESSAGES: &[(&str, &str, &str)] = &[\n");
    for (key, chinese_text) in &chinese.messages {
        let english_text = &english.messages[key];
        generated.push_str(&format!(
            "    ({}, {}, {}),\n",
            rust_string(key),
            rust_string(chinese_text),
            rust_string(english_text)
        ));
    }
    generated.push_str("];\n\n");

    generated.push_str("macro_rules! localized_text {\n");
    for key in chinese.messages.keys() {
        let key_literal = rust_string(key);
        generated.push_str(&format!(
            "    ({key_literal}) => {{{{ $crate::i18n::text({key_literal}) }}}};\n"
        ));
    }
    generated.push_str(
        "    ($key:literal) => { compile_error!(concat!(\"unknown localization key: \", $key)) };\n}\n\n",
    );

    generated.push_str("macro_rules! localized_format {\n");
    for (key, chinese_text) in &chinese.messages {
        let key_literal = rust_string(key);
        let chinese_literal = rust_string(chinese_text);
        let english_literal = rust_string(&english.messages[key]);
        generated.push_str(&format!(
            "    ({key_literal}) => {{{{ match $crate::i18n::current_language() {{\n        $crate::i18n::Language::English => ::std::format!({english_literal}),\n        $crate::i18n::Language::Chinese => ::std::format!({chinese_literal}),\n    }} }}}};\n"
        ));
        generated.push_str(&format!(
            "    ({key_literal}, $($args:tt)*) => {{{{ match $crate::i18n::current_language() {{\n        $crate::i18n::Language::English => ::std::format!({english_literal}, $($args)*),\n        $crate::i18n::Language::Chinese => ::std::format!({chinese_literal}, $($args)*),\n    }} }}}};\n"
        ));
    }
    generated.push_str(
        "    ($key:literal) => { compile_error!(concat!(\"unknown localization key: \", $key)) };\n    ($key:literal, $($args:tt)*) => { compile_error!(concat!(\"unknown localization key: \", $key)) };\n}\n",
    );

    let output_path = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("i18n_generated.rs");
    fs::write(output_path, generated).expect("failed to write generated localization catalog");

    #[cfg(target_os = "windows")]
    {
        winres::WindowsResource::new()
            .set_icon("assets/VD.ico")
            .set_manifest_file("app.manifest")
            .compile()
            .expect("failed to embed the application icon and manifest");
    }
}
