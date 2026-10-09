//! Minimal RFC 4180 CSV: quoted fields with `""` escapes, commas and line
//! breaks inside quotes, LF or CRLF row ends.

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CsvError {
    #[error("unterminated quoted field starting on line {line}")]
    UnterminatedQuote { line: usize },
}

/// Parse CSV text into rows of fields. A trailing line break doesn't add an
/// empty row. A quote inside an unquoted field is kept as a character.
pub fn parse(text: &str) -> Result<Vec<Vec<String>>, CsvError> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut line = 1;
    // Whether the current row has any content yet (for the trailing newline).
    let mut started = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if field.is_empty() => {
                let start = line;
                loop {
                    match chars.next() {
                        Some('"') if chars.peek() == Some(&'"') => {
                            chars.next();
                            field.push('"');
                        }
                        Some('"') => break,
                        Some(c) => {
                            if c == '\n' {
                                line += 1;
                            }
                            field.push(c);
                        }
                        None => return Err(CsvError::UnterminatedQuote { line: start }),
                    }
                }
                started = true;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                started = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                line += 1;
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                started = false;
            }
            c => {
                field.push(c);
                started = true;
            }
        }
    }
    if started {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

/// Append one row, quoting fields that need it, ending with CRLF.
pub fn write_row(out: &mut String, fields: &[&str]) {
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let quote = field.contains([',', '"', '\n', '\r'])
            || field.starts_with(char::is_whitespace)
            || field.ends_with(char::is_whitespace);
        if quote {
            out.push('"');
            out.push_str(&field.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(field);
        }
    }
    out.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_commas_and_newlines() {
        let text = "a,b\r\n\"x, y\",\"say \"\"hi\"\"\"\n\"multi\nline\",\n";
        assert_eq!(
            parse(text).unwrap(),
            vec![
                vec!["a", "b"],
                vec!["x, y", "say \"hi\""],
                vec!["multi\nline", ""],
            ]
        );
    }

    #[test]
    fn last_row_without_newline_and_empty_lines() {
        assert_eq!(
            parse("a\n\nb").unwrap(),
            vec![vec!["a"], vec![""], vec!["b"]]
        );
        assert!(parse("").unwrap().is_empty());
        assert_eq!(parse("x\"y,z").unwrap(), vec![vec!["x\"y", "z"]]);
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert_eq!(
            parse("a\n\"open,\nb").unwrap_err(),
            CsvError::UnterminatedQuote { line: 2 }
        );
    }

    #[test]
    fn write_then_parse_round_trips() {
        let rows = [
            vec!["term", "translation"],
            vec!["in-context learning", "上下文学习"],
            vec!["a, b", "say \"x\""],
            vec![" padded ", "line\nbreak"],
            vec!["GNN", ""],
        ];
        let mut out = String::new();
        for r in &rows {
            write_row(&mut out, r);
        }
        assert_eq!(parse(&out).unwrap(), rows);
    }
}
