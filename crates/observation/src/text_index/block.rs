/// UTF-8 lowercase can expand a character (for example U+0130). Keep one mapping
/// for each resulting character so a normalized hit always points to original
/// bytes rather than a byte offset in a fabricated normalized string.
pub(crate) fn fold(text: &str) -> (String, Vec<u8>) {
    let mut folded = String::new();
    let mut offsets = Vec::new();
    for (original, character) in text.char_indices() {
        for lower in character.to_lowercase() {
            offsets.extend_from_slice(&(folded.len() as u32).to_le_bytes());
            offsets.extend_from_slice(&(original as u32).to_le_bytes());
            folded.push(lower);
        }
    }
    offsets.extend_from_slice(&(folded.len() as u32).to_le_bytes());
    offsets.extend_from_slice(&(text.len() as u32).to_le_bytes());
    (folded, offsets)
}

pub(crate) fn original_offset(offsets: &[u8], normalized: usize) -> Option<u64> {
    if !offsets.len().is_multiple_of(8) {
        return None;
    }
    let entries = offsets.len() / 8;
    let mut low = 0;
    let mut high = entries;
    while low < high {
        let mid = low + (high - low) / 2;
        let position = u32::from_le_bytes(offsets[mid * 8..mid * 8 + 4].try_into().ok()?) as usize;
        if position < normalized {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    if low >= entries {
        return None;
    }
    let at = low * 8;
    if u32::from_le_bytes(offsets[at..at + 4].try_into().ok()?) as usize != normalized {
        return None;
    }
    Some(u64::from(u32::from_le_bytes(
        offsets[at + 4..at + 8].try_into().ok()?,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_expansion_and_multibyte_text_keep_original_offsets() {
        let (text, offsets) = fold("甲İ乙 Hello");
        assert_eq!(text, "甲i\u{307}乙 hello");
        assert_eq!(original_offset(&offsets, text.find('乙').unwrap()), Some(5));
        assert_eq!(original_offset(&offsets, text.find('h').unwrap()), Some(9));
        assert_eq!(original_offset(&offsets, 1), None);
    }
}
