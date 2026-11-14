// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A parse error for a settings file, without the file's values.
//!
//! `serde_json::Error`'s own text quotes the offending value ("invalid type:
//! string \"...\", expected a boolean"), so a credential in a mistyped field of
//! a hand-edited file would reach the log, the error string and the webview.
//! This keeps the kind and the position, which is what a user needs to find
//! the problem in the file.

/// The kind and position of `e`, never the value that caused it.
pub fn describe_json_error(e: &serde_json::Error) -> String {
    let kind = match e.classify() {
        serde_json::error::Category::Io => "read error",
        serde_json::error::Category::Syntax => "not valid JSON",
        serde_json::error::Category::Data => "a value has the wrong type or shape",
        serde_json::error::Category::Eof => "the file ends early",
    };
    format!("{kind} at line {} column {}", e.line(), e.column())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Settings {
        enabled: bool,
    }

    #[test]
    fn the_description_keeps_the_position_and_drops_the_value() {
        let err =
            serde_json::from_str::<Settings>(r#"{"enabled": "SENTINEL-SECRET"}"#).unwrap_err();
        // The raw text does carry the value: that is what this guards against.
        assert!(err.to_string().contains("SENTINEL-SECRET"));
        let described = describe_json_error(&err);
        assert!(!described.contains("SENTINEL-SECRET"), "{described}");
        assert_eq!(
            described,
            "a value has the wrong type or shape at line 1 column 29"
        );
    }
}
