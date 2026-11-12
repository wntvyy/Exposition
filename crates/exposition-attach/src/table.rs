//! Table grids parsed from CSV (RFC 4180).

use crate::AttachError;

/// A rectangular table: one header row plus body rows, all plain text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableGrid {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl TableGrid {
    /// Parse CSV; the first record is the header. Short rows are padded.
    pub fn from_csv(text: &str) -> Result<Self, AttachError> {
        let mut records = parse_csv(text)?;
        if records.is_empty() {
            return Err(AttachError::InvalidTable("the table is empty".into()));
        }
        let width = records.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            return Err(AttachError::InvalidTable("the table has no columns".into()));
        }
        for record in &mut records {
            record.resize(width, String::new());
        }
        let header = records.remove(0);
        Ok(Self { header, rows: records })
    }

    pub fn columns(&self) -> usize {
        self.header.len()
    }

    /// Columns whose body cells are all numbers (empty cells allowed) align right.
    pub fn numeric_columns(&self) -> Vec<bool> {
        (0..self.columns())
            .map(|c| {
                let mut any = false;
                for row in &self.rows {
                    let cell = row[c].trim();
                    if cell.is_empty() {
                        continue;
                    }
                    if cell.replace(',', "").parse::<f64>().is_err() {
                        return false;
                    }
                    any = true;
                }
                any
            })
            .collect()
    }
}

fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, AttachError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    let mut at_field_start = true;
    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                other => field.push(other),
            }
            continue;
        }
        match ch {
            '"' if at_field_start => {
                in_quotes = true;
                at_field_start = false;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                at_field_start = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                at_field_start = true;
            }
            other => {
                field.push(other);
                at_field_start = false;
            }
        }
    }
    if in_quotes {
        return Err(AttachError::InvalidTable("the table has an unterminated quoted field".into()));
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_fields_and_pads_rows() {
        let grid =
            TableGrid::from_csv("\u{feff}a,\"b,c\",d\r\n1,\"say \"\"hi\"\"\"\n2,\"multi\nline\",3,extra\n").unwrap();
        assert_eq!(grid.header, vec!["a", "b,c", "d", ""]);
        assert_eq!(grid.rows[0], vec!["1", "say \"hi\"", "", ""]);
        assert_eq!(grid.rows[1], vec!["2", "multi\nline", "3", "extra"]);
        assert!(TableGrid::from_csv("a,\"b").is_err());
        assert!(TableGrid::from_csv("").is_err());
    }

    #[test]
    fn detects_numeric_columns() {
        let grid = TableGrid::from_csv(",n,name,x\n0,1,a,1.5\n1,2,b,\n2,1e3,c,-2\n").unwrap();
        assert_eq!(grid.numeric_columns(), vec![true, true, false, true]);
    }
}
