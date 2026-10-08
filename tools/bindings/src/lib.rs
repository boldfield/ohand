//! Reproducible C header generation for the Rust core ABI, and verification that a built
//! static library exports exactly the functions the header declares.

use cbindgen::{Builder, Config};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

pub const EXPORT_PREFIX: &str = "ohand_";
pub const CONSTANT_PREFIX: &str = "OHAND_";

pub fn generate_header(binding_crate: &Path, config_file: &Path) -> Result<String, String> {
    let config = Config::from_file(config_file).map_err(|error| error.to_string())?;
    let bindings = Builder::new()
        .with_config(config)
        .with_crate(binding_crate)
        .generate()
        .map_err(|error| error.to_string())?;
    let mut header_bytes = Vec::new();
    bindings.write(&mut header_bytes);
    let header = String::from_utf8(header_bytes).map_err(|error| error.to_string())?;
    Ok(retain_abi_constants(&header))
}

/// cbindgen exports every public constant of the core crate once that crate is a binding
/// source. Only constants named `OHAND_*` belong to the ABI, so others are dropped together
/// with their documentation.
fn retain_abi_constants(header: &str) -> String {
    let blocks: Vec<&str> = header
        .split("\n\n")
        .filter(|block| {
            let defined_name = block
                .lines()
                .last()
                .and_then(|line| line.strip_prefix("#define "))
                .and_then(|definition| definition.split_whitespace().next());
            defined_name.is_none_or(|name| name.starts_with(CONSTANT_PREFIX))
        })
        .collect();
    blocks.join("\n\n")
}

fn without_comments_and_preprocessor_lines(header: &str) -> String {
    let mut stripped = String::new();
    let mut remaining = header;
    while let Some(comment_start) = remaining.find("/*") {
        stripped.push_str(&remaining[..comment_start]);
        match remaining[comment_start..].find("*/") {
            Some(comment_length) => remaining = &remaining[comment_start + comment_length + 2..],
            None => return stripped,
        }
    }
    stripped.push_str(remaining);
    stripped
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_identifier_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// Names of the functions a generated header declares, in sorted order.
pub fn declared_functions(header: &str) -> BTreeSet<String> {
    let declarations = without_comments_and_preprocessor_lines(header);
    let mut names = BTreeSet::new();
    for statement in declarations.split(';') {
        if statement.trim_start().starts_with("typedef") {
            continue;
        }
        let Some(parenthesis) = statement.find('(') else {
            continue;
        };
        let before_parameters = statement[..parenthesis].trim_end();
        let name: String = before_parameters
            .chars()
            .rev()
            .take_while(|character| is_identifier_character(*character))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !name.is_empty() {
            names.insert(name);
        }
    }
    names
}

/// Names of the code symbols exported by a static library, from `nm -g` output. Mach-O
/// prefixes C symbols with an underscore; ELF does not.
pub fn exported_functions(nm_output: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in nm_output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [.., symbol_type, symbol_name] = fields.as_slice() else {
            continue;
        };
        if *symbol_type != "T" {
            continue;
        }
        let unprefixed = symbol_name.strip_prefix('_').unwrap_or(symbol_name);
        if unprefixed.starts_with(EXPORT_PREFIX) {
            names.insert(unprefixed.to_string());
        }
    }
    names
}

pub fn compare_header_with_library(
    declared: &BTreeSet<String>,
    exported: &BTreeSet<String>,
) -> Result<(), String> {
    let unprefixed: Vec<&String> = declared
        .iter()
        .filter(|name| !name.starts_with(EXPORT_PREFIX))
        .collect();
    if !unprefixed.is_empty() {
        return Err(format!(
            "exported functions must be named {EXPORT_PREFIX}*: {unprefixed:?}"
        ));
    }
    let missing_from_library: Vec<&String> = declared.difference(exported).collect();
    let missing_from_header: Vec<&String> = exported.difference(declared).collect();
    if declared.is_empty() {
        return Err("the header declares no ohand_ functions".to_string());
    }
    if missing_from_library.is_empty() && missing_from_header.is_empty() {
        return Ok(());
    }
    Err(format!(
        "header and library disagree; declared but not exported: {missing_from_library:?}; \
         exported but not declared: {missing_from_header:?}"
    ))
}

pub fn check_library_matches_header(header_file: &Path, library_file: &Path) -> Result<(), String> {
    let header = std::fs::read_to_string(header_file)
        .map_err(|error| format!("read {}: {error}", header_file.display()))?;
    let nm_output = Command::new("nm")
        .arg("-g")
        .arg(library_file)
        .output()
        .map_err(|error| format!("run nm: {error}"))?;
    if !nm_output.status.success() {
        return Err(format!(
            "nm failed on {}: {}",
            library_file.display(),
            String::from_utf8_lossy(&nm_output.stderr)
        ));
    }
    compare_header_with_library(
        &declared_functions(&header),
        &exported_functions(&String::from_utf8_lossy(&nm_output.stdout)),
    )
}
