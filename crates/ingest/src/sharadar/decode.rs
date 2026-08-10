//! Turning one page of the vendor's envelope into values, bound by column name.
//!
//! The vendor sends rows as positional arrays and the column list separately.
//! Nothing here indexes a row with a literal number. Every read goes through a
//! name, and a name that is not in the page's column list is an error that says
//! which name, rather than a value silently taken from the neighbouring column.

use std::collections::BTreeMap;
use std::str::FromStr;

use jiff::civil::Date;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use super::malformed;
use crate::provider::SourceError;

/// One page of a datatables response, still in vendor shape.
pub(crate) struct Page {
    /// Column name to its position in every row of this page.
    ///
    /// Built per page rather than once per query, because the binding is only
    /// valid for the rows that arrived with it.
    index: BTreeMap<String, usize>,
    rows: Vec<Vec<Value>>,
    /// `meta.next_cursor_id`, absent when this is the last page.
    next_cursor: Option<String>,
}

// The wire envelope. Only the fields this client reads are declared; the vendor
// sends more and serde ignores the rest, which is what lets a new column appear
// without breaking the parse.
#[derive(Deserialize)]
struct Envelope {
    datatable: Datatable,
    #[serde(default)]
    meta: Meta,
}

#[derive(Deserialize)]
struct Datatable {
    columns: Vec<ColumnSpec>,
    data: Vec<Vec<Value>>,
}

#[derive(Deserialize)]
struct ColumnSpec {
    name: String,
}

#[derive(Deserialize, Default)]
struct Meta {
    #[serde(default)]
    next_cursor_id: Option<String>,
}

impl Page {
    /// Parse one response body.
    pub(crate) fn parse(body: &str) -> Result<Self, SourceError> {
        // The parse error is reported without the body. serde_json's message
        // gives a line and column, which is enough to locate the problem, and
        // quoting the input risks echoing back a request that carried the key.
        let envelope: Envelope = serde_json::from_str(body)
            .map_err(|error| malformed(format!("the response was not a datatable: {error}")))?;

        // The response's own column list is the only thing that says where a
        // value lives. Building the index here, from `datatable.columns` and
        // nothing else, is what makes every later read name-bound.
        let mut index = BTreeMap::new();
        for (position, column) in envelope.datatable.columns.iter().enumerate() {
            if index.insert(column.name.clone(), position).is_some() {
                return Err(malformed(format!(
                    "the response declared the column {:?} twice, so no value for it \
                     can be located unambiguously",
                    column.name
                )));
            }
        }

        Ok(Page {
            index,
            rows: envelope.datatable.data,
            // The vendor sends null for the last page. An empty string would
            // mean the same thing and is treated the same, because a cursor
            // that is present but empty would otherwise re-request page one
            // forever.
            next_cursor: envelope
                .meta
                .next_cursor_id
                .filter(|cursor| !cursor.is_empty()),
        })
    }

    /// The cursor to pass back as `qopts.cursor_id`, if there is another page.
    pub(crate) fn next_cursor(&self) -> Option<&str> {
        self.next_cursor.as_deref()
    }

    pub(crate) fn rows(&self) -> impl Iterator<Item = RowReader<'_>> {
        self.rows.iter().map(|row| RowReader {
            index: &self.index,
            row,
        })
    }
}

/// One row, readable only by column name.
pub(crate) struct RowReader<'a> {
    index: &'a BTreeMap<String, usize>,
    row: &'a [Value],
}

impl RowReader<'_> {
    /// The raw cell for a column, or an error naming the column.
    fn cell(&self, column: &str) -> Result<&Value, SourceError> {
        let position = self.index.get(column).ok_or_else(|| {
            malformed(format!(
                "the response has no column named {column:?}; it carried {}",
                self.column_names()
            ))
        })?;

        // A row shorter than the column list is the vendor contradicting
        // itself. Reading a shorter row against the same index would take
        // every later value from the wrong column.
        self.row.get(*position).ok_or_else(|| {
            malformed(format!(
                "the column {column:?} is at position {position} but the row holds only {} values",
                self.row.len()
            ))
        })
    }

    fn column_names(&self) -> String {
        self.index
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The exact text of a cell that holds either a string or a number.
    ///
    /// The vendor mixes the two forms within one row: large integers arrive
    /// quoted while ratios arrive bare. Both are returned as the characters
    /// that were on the wire, so nothing downstream has to know which form it
    /// got, and no number passes through `f64` on the way.
    pub(crate) fn token(&self, column: &str) -> Result<&str, SourceError> {
        match self.cell(column)? {
            Value::String(text) => Ok(text),
            // `Number::as_str` exists only because the workspace enables
            // serde_json's `arbitrary_precision`. Without it this would be a
            // parsed f64 and the original digits would already be gone.
            Value::Number(number) => Ok(number.as_str()),
            other => Err(malformed(format!(
                "the column {column:?} holds {}, and a string or a number was expected",
                describe(other)
            ))),
        }
    }

    /// True when the column is present and holds JSON null.
    fn is_null(&self, column: &str) -> Result<bool, SourceError> {
        Ok(self.cell(column)?.is_null())
    }

    /// A column that must hold a string.
    pub(crate) fn text(&self, column: &str) -> Result<&str, SourceError> {
        match self.cell(column)? {
            Value::String(text) => Ok(text),
            other => Err(malformed(format!(
                "the column {column:?} holds {}, and a string was expected",
                describe(other)
            ))),
        }
    }

    /// A column that holds a string or null.
    pub(crate) fn optional_text(&self, column: &str) -> Result<Option<&str>, SourceError> {
        if self.is_null(column)? {
            return Ok(None);
        }
        self.text(column).map(Some)
    }

    /// A column that must hold a number, in either the bare or the quoted form.
    pub(crate) fn decimal(&self, column: &str) -> Result<Decimal, SourceError> {
        let token = self.token(column)?;

        // `from_str_exact` rather than `from_str`. The latter rounds when a
        // token carries more precision than Decimal can hold, and a price that
        // silently rounds is indistinguishable from a correct one. Refusing is
        // the only outcome that stays honest.
        Decimal::from_str_exact(token)
            .or_else(|_| Decimal::from_scientific(token))
            .map_err(|error| {
                malformed(format!(
                    "the column {column:?} holds {token:?}, which is not a number \
                     this crate can hold exactly: {error}"
                ))
            })
    }

    /// A column that must hold an ISO 8601 date.
    pub(crate) fn date(&self, column: &str) -> Result<Date, SourceError> {
        let text = self.text(column)?;
        Date::from_str(text).map_err(|error| {
            malformed(format!(
                "the column {column:?} holds {text:?}, which is not a calendar date: {error}"
            ))
        })
    }

    /// A column that holds an ISO 8601 date or null.
    pub(crate) fn optional_date(&self, column: &str) -> Result<Option<Date>, SourceError> {
        if self.is_null(column)? {
            return Ok(None);
        }
        self.date(column).map(Some)
    }
}

/// Name a JSON value's kind for an error, without quoting the value itself.
fn describe(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
