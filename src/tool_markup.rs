/// The end of a complete XML-like opening tag.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpeningTag {
    pub(crate) end: usize,
    pub(crate) self_closing: bool,
}

#[allow(dead_code)] // consumed by the following incremental sanitizer layer
pub(crate) const TAGS: [&str; 7] = [
    "function_call",
    "tool_calls",
    "parameter",
    "function",
    "invoke",
    "think",
    "tool",
];

#[allow(dead_code)] // wired when the state-machine layer lands
pub(crate) fn recognized_tag(input: &str) -> Option<&'static str> {
    TAGS.into_iter().find(|tag| {
        let prefix = format!("<{tag}");
        input
            .get(..prefix.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(&prefix))
            && input
                .as_bytes()
                .get(prefix.len())
                .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'))
    })
}

/// Finds an opening tag delimiter without treating `>` in a quoted attribute
/// as structural. This is deliberately independent of response conversion so
/// stream and non-stream adapters share exactly the same token contract.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
pub(crate) fn opening_tag(input: &str) -> Option<OpeningTag> {
    let mut quote = None;
    let mut escaped = false;
    for (offset, byte) in input.as_bytes().iter().copied().enumerate() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'\"') {
            quote = Some(byte);
        } else if byte == b'>' {
            let end = offset + 1;
            return Some(OpeningTag {
                end,
                self_closing: input[..offset].trim_end().ends_with('/'),
            });
        }
    }
    None
}

/// A complete recognized tool-markup tag found in a content fragment.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagToken {
    Opening {
        tag: &'static str,
        start: usize,
        end: usize,
        self_closing: bool,
    },
    Closing {
        tag: &'static str,
        start: usize,
        end: usize,
    },
}

/// Finds the next complete recognized tag, preserving quoted `>` bytes inside
/// opening-tag attributes. Incomplete tags intentionally return `None` so a
/// streaming caller can retain them for the next content chunk.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
pub(crate) fn next_tag(input: &str) -> Option<TagToken> {
    for (start, _) in input.match_indices('<') {
        let candidate = &input[start..];
        if let Some(tag) = recognized_tag(candidate) {
            let opening = opening_tag(candidate)?;
            return Some(TagToken::Opening {
                tag,
                start,
                end: start + opening.end,
                self_closing: opening.self_closing,
            });
        }

        let Some(closing) = candidate.strip_prefix("</") else {
            continue;
        };
        for tag in TAGS {
            if !closing
                .get(..tag.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(tag))
            {
                continue;
            }
            let rest = &closing[tag.len()..];
            let end = rest.find('>')?;
            if !rest[..end].trim().is_empty() {
                continue;
            }
            return Some(TagToken::Closing {
                tag,
                start,
                end: start + 2 + tag.len() + end + 1,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::TagToken;
    use super::next_tag;
    use super::opening_tag;
    use super::recognized_tag;

    #[test]
    fn quoted_attribute_delimiter_is_not_a_tag_delimiter() {
        let tag = opening_tag("<parameter note=\"a > b\"/>After").expect("complete tag");
        assert_eq!(tag.end, "<parameter note=\"a > b\"/>".len());
        assert!(tag.self_closing);
    }

    #[test]
    fn incomplete_quoted_attribute_stays_incomplete() {
        assert!(opening_tag("<parameter note=\"a >").is_none());
    }

    #[test]
    fn recognized_tags_are_case_insensitive_and_require_a_tag_boundary() {
        assert_eq!(
            recognized_tag("<FUNCTION_CALL name=\"run\">"),
            Some("function_call")
        );
        assert_eq!(recognized_tag("<parameter/>"), Some("parameter"));
        assert_eq!(recognized_tag("<toolbox>"), None);
        assert_eq!(recognized_tag("<functionality>"), None);
        assert_eq!(recognized_tag("<invoke"), None);
    }

    #[test]
    fn next_tag_keeps_quoted_delimiters_and_validates_closing_tags() {
        assert_eq!(
            next_tag("before <parameter note=\"a > b\"/>After"),
            Some(TagToken::Opening {
                tag: "parameter",
                start: 7,
                end: 32,
                self_closing: true,
            })
        );
        assert_eq!(
            next_tag("</FUNCTION >After"),
            Some(TagToken::Closing {
                tag: "function",
                start: 0,
                end: 12,
            })
        );
        assert_eq!(next_tag("</function extra>"), None);
        assert_eq!(next_tag("<parameter note=\"unterminated"), None);
    }
}
