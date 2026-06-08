//! The boundary that turns a markdown file's bytes into typed Rust values.
//!
//! On-disk format is "YAML frontmatter + markdown body" — the source of truth.
//! This module does two things: (1) `split` peels the `---`-fenced frontmatter
//! block off the front of a file, and (2) `FrontMatter` types those fields so
//! the indexer can read them. Every field is optional on disk; the indexer
//! supplies fallbacks (title from H1/filename, id generated, timestamps from
//! the filesystem). See `Phase1_Format_Decisions.md` for which fields are
//! authoritative.

use serde::Deserialize;

/// The typed frontmatter block. `#[serde(default)]` means a missing key becomes
/// the type's default (empty list, `false`) rather than a parse error — files
/// in the wild won't all have every field.
#[derive(Debug, Default, Deserialize)]
pub struct FrontMatter {
    pub id: Option<String>,
    pub title: Option<String>,
    pub created: Option<String>,
    pub updated: Option<String>,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Result of splitting a file: the raw YAML (if any) and the body after it.
/// Borrows from the input — no allocation, the indexer owns the original string.
pub struct Split<'a> {
    pub frontmatter: Option<&'a str>,
    pub body: &'a str,
}

/// Peel a leading `---\n … \n---\n` frontmatter block off `content`.
///
/// Rules: the opening fence must be the very first line. The block ends at the
/// first line that is exactly `---`. If there's no opening fence, or it's never
/// closed, the whole file is treated as body (fail safe — never lose content).
pub fn split(content: &str) -> Split<'_> {
    // The opening fence has to be line 1, or there is no frontmatter.
    if !(content.starts_with("---\n") || content.starts_with("---\r\n")) {
        return Split { frontmatter: None, body: content };
    }
    // Offset just past the opening fence line.
    let after_open = content.find('\n').map(|i| i + 1).unwrap_or(content.len());

    // Walk the remaining lines (keeping their newlines via split_inclusive so
    // byte offsets stay exact) until we hit a closing `---` line.
    let mut offset = after_open;
    for line in content[after_open..].split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let frontmatter = &content[after_open..offset];
            let body_start = offset + line.len();
            let body = content.get(body_start..).unwrap_or("");
            return Split { frontmatter: Some(frontmatter), body };
        }
        offset += line.len();
    }

    // Unterminated frontmatter — treat the whole file as body.
    Split { frontmatter: None, body: content }
}
