use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Options, Parser, Tag};
use regex::Regex;
use sha2::{Digest, Sha256};

use crate::model::{ParsedChunk, ParsedLink, ParsedNote};

pub fn parse_note(vault: &Path, path: &Path) -> Result<ParsedNote> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
    let metadata = fs::metadata(path)?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let hash = hex::encode(Sha256::digest(&bytes));
    let relative = path
        .strip_prefix(vault)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let title = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_owned();
    let (frontmatter, body) = split_frontmatter(&text);

    Ok(ParsedNote {
        path: relative,
        title,
        frontmatter,
        chunks: split_chunks(body),
        links: extract_links(body),
        body: body.to_owned(),
        mtime,
        size: metadata.len(),
        hash,
    })
}

fn split_frontmatter(text: &str) -> (Option<serde_json::Value>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, text);
    };
    let Some(end) = rest.find("\n---\n") else {
        return (None, text);
    };
    let yaml = &rest[..end];
    let body = &rest[end + 5..];
    let value = serde_yaml::from_str::<serde_yaml::Value>(yaml)
        .ok()
        .and_then(|value| serde_json::to_value(value).ok());
    (value, body)
}

fn split_chunks(body: &str) -> Vec<ParsedChunk> {
    let heading_re = Regex::new(r"(?m)^(#{1,6})[ \t]+(.+?)[ \t]*$").unwrap();
    let mut sections = Vec::new();
    let mut last = 0;
    let mut heading = None;

    for captures in heading_re.captures_iter(body) {
        let matched = captures.get(0).unwrap();
        push_section(&mut sections, heading.take(), &body[last..matched.start()]);
        heading = Some(captures[2].trim().to_owned());
        last = matched.end();
    }
    push_section(&mut sections, heading, &body[last..]);

    if sections.is_empty() && !body.trim().is_empty() {
        sections.push(ParsedChunk {
            ordinal: 0,
            heading: None,
            body: body.trim().to_owned(),
        });
    }

    for (ordinal, chunk) in sections.iter_mut().enumerate() {
        chunk.ordinal = ordinal;
    }
    sections
}

fn push_section(chunks: &mut Vec<ParsedChunk>, heading: Option<String>, text: &str) {
    const MAX_CHARS: usize = 2400;
    let text = text.trim();
    if text.is_empty() {
        return;
    }

    let mut current = String::new();
    for paragraph in text.split("\n\n") {
        if !current.is_empty() && current.chars().count() + paragraph.chars().count() > MAX_CHARS {
            chunks.push(ParsedChunk {
                ordinal: 0,
                heading: heading.clone(),
                body: current.trim().to_owned(),
            });
            current.clear();
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(paragraph);
    }
    if !current.trim().is_empty() {
        chunks.push(ParsedChunk {
            ordinal: 0,
            heading,
            body: current.trim().to_owned(),
        });
    }
}

fn extract_links(body: &str) -> Vec<ParsedLink> {
    let wiki_re = Regex::new(r"(!)?\[\[([^\[\]]+)\]\]").unwrap();
    let mut links: Vec<ParsedLink> = wiki_re
        .captures_iter(body)
        .filter_map(|captures| {
            let raw = captures.get(2)?.as_str().trim().to_owned();
            let destination = raw.split('|').next()?.trim().to_owned();
            let (target, heading) = destination
                .split_once('#')
                .map(|(target, heading)| (target.trim(), Some(heading.trim().to_owned())))
                .unwrap_or((destination.as_str(), None));
            if target.is_empty() {
                return None;
            }
            Some(ParsedLink {
                raw_target: raw,
                target: normalize_target(target),
                heading,
                is_embed: captures.get(1).is_some(),
            })
        })
        .collect();

    links.extend(Parser::new_ext(body, Options::all()).filter_map(|event| {
        let (destination, is_embed) = match event {
            Event::Start(Tag::Link { dest_url, .. }) => (dest_url, false),
            Event::Start(Tag::Image { dest_url, .. }) => (dest_url, true),
            _ => return None,
        };
        let raw = destination.trim().to_owned();
        if is_external_destination(&raw) {
            return None;
        }
        let decoded = percent_decode_str(&raw).decode_utf8_lossy().into_owned();
        let (target, heading) = decoded
            .split_once('#')
            .map(|(target, heading)| (target.trim(), Some(heading.trim().to_owned())))
            .unwrap_or((decoded.as_str(), None));
        if target.is_empty() {
            return None;
        }
        Some(ParsedLink {
            raw_target: raw,
            target: normalize_target(target),
            heading,
            is_embed,
        })
    }));
    let mut seen = HashSet::new();
    links.retain(|link| seen.insert((link.target.clone(), link.heading.clone(), link.is_embed)));
    links
}

pub fn normalize_target(target: &str) -> String {
    let mut path = PathBuf::from(target.trim());
    if path.extension().is_some_and(|extension| extension == "md") {
        path.set_extension("");
    }
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_matches('/')
        .to_lowercase()
}

fn is_external_destination(destination: &str) -> bool {
    let lowercase = destination.to_ascii_lowercase();
    lowercase.starts_with("http:")
        || lowercase.starts_with("https:")
        || lowercase.starts_with("mailto:")
        || lowercase.starts_with("data:")
        || lowercase.starts_with("file:")
        || lowercase.starts_with("//")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arbitrary_frontmatter() {
        let text = "---\ncustom:\n  nested: 42\n旗: true\n---\n# Body\nText";
        let (frontmatter, body) = split_frontmatter(text);
        let value = frontmatter.unwrap();
        assert_eq!(value["custom"]["nested"], 42);
        assert_eq!(value["旗"], true);
        assert!(body.contains("# Body"));
    }

    #[test]
    fn extracts_wiki_links_without_property_assumptions() {
        let links = extract_links("See [[Folder/Note#Section|label]] and ![[asset.png]].");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "folder/note");
        assert_eq!(links[0].heading.as_deref(), Some("Section"));
        assert!(links[1].is_embed);
    }

    #[test]
    fn extracts_markdown_links_and_decodes_paths() {
        let links = extract_links(
            "[note](<../Folder/A Note.md#Part> \"title\") ![embed](asset.png) [web](https://example.com)",
        );
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "../folder/a note");
        assert_eq!(links[0].heading.as_deref(), Some("Part"));
        assert!(links[1].is_embed);
    }
}
