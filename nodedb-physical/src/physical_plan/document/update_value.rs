// SPDX-License-Identifier: Apache-2.0

//! Right-hand side of an UPDATE ... SET field = <...> assignment.

/// Right-hand side of an UPDATE ... SET field = <...> assignment.
///
/// The planner turns each assignment into one of these before it crosses
/// the SPSC bridge:
///
/// - `Literal` — pre-encoded msgpack bytes for constant RHS. This is the
///   fast path: the Data Plane can merge these at the binary level for
///   non-strict collections without decoding the current row.
/// - `Expr` — a `SqlExpr` that must be evaluated against the *current*
///   document at apply time. Used for arithmetic (`col + 1`), functions
///   (`LOWER(col)`, `NOW()`), `CASE`, concatenation, and anything else
///   whose result depends on the row being updated.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum UpdateValue {
    Literal(Vec<u8>),
    Expr(nodedb_query::expr::SqlExpr),
}

impl zerompk::ToMessagePack for UpdateValue {
    fn write<W: zerompk::Write>(&self, writer: &mut W) -> zerompk::Result<()> {
        writer.write_array_len(2)?;
        match self {
            UpdateValue::Literal(bytes) => {
                writer.write_u8(0)?;
                bytes.write(writer)
            }
            UpdateValue::Expr(expr) => {
                writer.write_u8(1)?;
                expr.write(writer)
            }
        }
    }
}

impl<'a> zerompk::FromMessagePack<'a> for UpdateValue {
    fn read<R: zerompk::Read<'a>>(reader: &mut R) -> zerompk::Result<Self> {
        reader.check_array_len(2)?;
        let tag = reader.read_u8()?;
        match tag {
            0 => Ok(UpdateValue::Literal(Vec::<u8>::read(reader)?)),
            1 => Ok(UpdateValue::Expr(nodedb_query::expr::SqlExpr::read(
                reader,
            )?)),
            _ => Err(zerompk::Error::InvalidMarker(tag)),
        }
    }
}
