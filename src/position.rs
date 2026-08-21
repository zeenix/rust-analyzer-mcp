//! Turning the positions the LSP speaks in into offsets into a file.
//!
//! A position is a line and a column, both counted from zero, and the column is counted in
//! UTF-16 code units -- not bytes, and not characters. For a line of ASCII all three agree, which
//! is why the difference is easy to miss and unforgiving when it bites: on a line with an
//! accented letter the byte offset runs ahead, and on one with an emoji the UTF-16 count does.

/// The byte offset of the LSP position (`line`, `character`) in `text`.
///
/// A position past the end of its line, or past the end of the text, gives the end of what it
/// points into: that is what editors do with one, and it keeps a rounding error in whoever asked
/// from becoming a panic here.
pub fn byte_offset(text: &str, line: u32, character: u32) -> usize {
    let mut offset = 0;

    for (number, line_text) in text.split_inclusive('\n').enumerate() {
        if number as u32 == line {
            return offset + column_offset(line_text, character);
        }
        offset += line_text.len();
    }

    // Past the last line, which a range ending at the end of the file does.
    text.len()
}

/// The byte offset of `character`, counted in UTF-16 code units, into one line.
fn column_offset(line: &str, character: u32) -> usize {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);

    let mut counted = 0;
    for (offset, character_here) in line.char_indices() {
        if counted >= character {
            return offset;
        }

        // A column that lands inside a character -- half an emoji -- is not an offset into
        // anything, so it points at the character it landed in.
        let after = counted + character_here.len_utf16() as u32;
        if after > character {
            return offset;
        }
        counted = after;
    }

    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "fn main() {\n    let x = 1;\n}\n";

    #[test]
    fn a_position_is_a_line_and_a_column() {
        assert_eq!(byte_offset(TEXT, 0, 0), 0);
        assert_eq!(byte_offset(TEXT, 0, 3), 3);
        assert_eq!(byte_offset(TEXT, 1, 4), "fn main() {\n    ".len());
        assert_eq!(byte_offset(TEXT, 2, 0), TEXT.len() - 2);
    }

    #[test]
    fn columns_are_counted_in_utf16_code_units() {
        // `é` is one code unit and two bytes; `🦀` is two code units and four bytes.
        let text = "let é = \"🦀\";";

        assert_eq!(byte_offset(text, 0, 4), "let ".len());
        assert_eq!(byte_offset(text, 0, 5), "let é".len());
        assert_eq!(byte_offset(text, 0, 9), "let é = \"".len());
        // The crab is two code units wide and four bytes long.
        assert_eq!(byte_offset(text, 0, 11), "let é = \"🦀".len());
    }

    #[test]
    fn a_column_inside_a_character_stops_before_it() {
        // Half an emoji is not an offset; the character it lands in is where it points.
        let text = "🦀🦀";

        assert_eq!(byte_offset(text, 0, 1), 0);
        assert_eq!(byte_offset(text, 0, 2), "🦀".len());
        assert_eq!(byte_offset(text, 0, 3), "🦀".len());
    }

    #[test]
    fn a_position_past_the_end_is_the_end() {
        assert_eq!(byte_offset(TEXT, 0, 999), "fn main() {".len());
        assert_eq!(byte_offset(TEXT, 99, 0), TEXT.len());
        assert_eq!(byte_offset("", 0, 0), 0);
    }

    #[test]
    fn line_endings_are_not_part_of_the_line() {
        let text = "one\r\ntwo\r\n";

        assert_eq!(byte_offset(text, 0, 99), "one".len());
        assert_eq!(byte_offset(text, 1, 0), "one\r\n".len());
        assert_eq!(byte_offset(text, 1, 3), "one\r\ntwo".len());
    }
}
