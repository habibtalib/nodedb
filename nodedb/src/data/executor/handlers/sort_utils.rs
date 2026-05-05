//! Shared msgpack-row sorting utilities used by scan handlers.

/// Sort msgpack-map rows by `(field, ascending)` keys. Decodes each row to
/// JSON, extracts the sort fields, and reorders the original msgpack bytes.
/// The `bool` matches the document scan convention (`true` = ascending).
///
/// Decode failures for individual rows are logged at debug level and treated
/// as `null` for comparison purposes, so they sort to the start/end rather
/// than causing the entire sort to fail.
pub(in crate::data::executor) fn sort_msgpack_rows(
    rows: &mut [Vec<u8>],
    sort_keys: &[(String, bool)],
) {
    let decoded: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| match nodedb_types::json_from_msgpack(r) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(err = %e, "msgpack decode failed during sort; treating row as null");
                serde_json::Value::Null
            }
        })
        .collect();

    let mut indices: Vec<usize> = (0..rows.len()).collect();
    indices.sort_by(|&a, &b| {
        for (field, asc) in sort_keys {
            let va = decoded[a].get(field).unwrap_or(&serde_json::Value::Null);
            let vb = decoded[b].get(field).unwrap_or(&serde_json::Value::Null);
            let ord = compare_json(va, vb);
            if ord != std::cmp::Ordering::Equal {
                return if *asc { ord } else { ord.reverse() };
            }
        }
        std::cmp::Ordering::Equal
    });

    let original: Vec<Vec<u8>> = rows.to_vec();
    for (dst, src) in indices.iter().enumerate() {
        rows[dst] = original[*src].clone();
    }
}

fn compare_json(a: &serde_json::Value, b: &serde_json::Value) -> std::cmp::Ordering {
    use serde_json::Value;
    match (a, b) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .partial_cmp(&y.as_f64())
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => a.to_string().cmp(&b.to_string()),
    }
}
