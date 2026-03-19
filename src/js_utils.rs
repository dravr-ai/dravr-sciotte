// ABOUTME: JavaScript string escaping utilities for safe embedding in CDP evaluate calls
// ABOUTME: Used by scraper and server to build JS snippets with user-provided values
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

/// Escape a string for safe embedding in JS double-quoted strings
pub fn escape_js_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Escape a CSS selector for embedding in JS double-quoted strings
pub fn escape_js_selector(s: &str) -> String {
    escape_js_string(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_plain_string() {
        assert_eq!(escape_js_string("hello"), "hello");
    }

    #[test]
    fn escape_quotes_and_backslashes() {
        assert_eq!(escape_js_string(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn escape_newlines() {
        assert_eq!(escape_js_string("line1\nline2\r"), "line1\\nline2\\r");
    }

    #[test]
    fn escape_selector_with_brackets() {
        assert_eq!(
            escape_js_selector(r#"input[name="email"]"#),
            r#"input[name=\"email\"]"#
        );
    }
}
