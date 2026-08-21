/// The end of a complete XML-like opening tag.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpeningTag {
    pub(crate) end: usize,
    pub(crate) self_closing: bool,
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

#[cfg(test)]
mod tests {
    use super::opening_tag;

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
}
