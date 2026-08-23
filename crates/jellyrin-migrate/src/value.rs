use anyhow::Context;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Row, postgres::PgRow, sqlite::SqliteRow};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::spec::{ColumnKind, ColumnSpec, JsonShape};

#[derive(Debug, Clone, PartialEq)]
pub enum TypedValue {
    Text(Option<String>),
    Bytes(Option<Vec<u8>>),
    Uuid(Option<Uuid>),
    Timestamp(Option<OffsetDateTime>),
    Bool(Option<bool>),
    I16(Option<i16>),
    I32(Option<i32>),
    I64(Option<i64>),
    F64(Option<f64>),
    Json(Option<Value>),
}

impl TypedValue {
    pub fn from_sqlite(row: &SqliteRow, column: &ColumnSpec) -> anyhow::Result<Self> {
        let value = match column.kind {
            ColumnKind::Text => Self::Text(row.try_get(column.source)?),
            ColumnKind::Bytes => Self::Bytes(row.try_get(column.source)?),
            ColumnKind::Uuid => Self::Uuid(
                row.try_get::<Option<String>, _>(column.source)?
                    .map(|raw| parse_uuid(&raw))
                    .transpose()?,
            ),
            ColumnKind::Timestamp => Self::Timestamp(
                row.try_get::<Option<String>, _>(column.source)?
                    .map(|raw| parse_timestamp(&raw))
                    .transpose()?,
            ),
            ColumnKind::Bool => Self::Bool(
                row.try_get::<Option<i64>, _>(column.source)?
                    .map(parse_bool)
                    .transpose()?,
            ),
            ColumnKind::I16 => Self::I16(
                row.try_get::<Option<i64>, _>(column.source)?
                    .map(|value| {
                        i16::try_from(value).context("integer does not fit PostgreSQL smallint")
                    })
                    .transpose()?,
            ),
            ColumnKind::I32 => Self::I32(
                row.try_get::<Option<i64>, _>(column.source)?
                    .map(|value| {
                        i32::try_from(value).context("integer does not fit PostgreSQL integer")
                    })
                    .transpose()?,
            ),
            ColumnKind::I64 => Self::I64(row.try_get(column.source)?),
            ColumnKind::F64 => Self::F64(row.try_get(column.source)?),
            ColumnKind::Json(shape) => Self::Json(
                row.try_get::<Option<String>, _>(column.source)?
                    .map(|raw| parse_json(&raw, shape))
                    .transpose()?,
            ),
        };
        anyhow::ensure!(
            column.nullable || !value.is_null(),
            "required value is NULL"
        );
        Ok(value)
    }

    pub fn from_postgres(row: &PgRow, column: &ColumnSpec) -> anyhow::Result<Self> {
        let value = match column.kind {
            ColumnKind::Text => Self::Text(row.try_get(column.target)?),
            ColumnKind::Bytes => Self::Bytes(row.try_get(column.target)?),
            ColumnKind::Uuid => Self::Uuid(row.try_get(column.target)?),
            ColumnKind::Timestamp => Self::Timestamp(row.try_get(column.target)?),
            ColumnKind::Bool => Self::Bool(row.try_get(column.target)?),
            ColumnKind::I16 => Self::I16(row.try_get(column.target)?),
            ColumnKind::I32 => Self::I32(row.try_get(column.target)?),
            ColumnKind::I64 => Self::I64(row.try_get(column.target)?),
            ColumnKind::F64 => Self::F64(row.try_get(column.target)?),
            ColumnKind::Json(_) => Self::Json(row.try_get(column.target)?),
        };
        anyhow::ensure!(
            column.nullable || !value.is_null(),
            "required value is NULL"
        );
        Ok(value)
    }

    pub fn is_null(&self) -> bool {
        match self {
            Self::Text(value) => value.is_none(),
            Self::Bytes(value) => value.is_none(),
            Self::Uuid(value) => value.is_none(),
            Self::Timestamp(value) => value.is_none(),
            Self::Bool(value) => value.is_none(),
            Self::I16(value) => value.is_none(),
            Self::I32(value) => value.is_none(),
            Self::I64(value) => value.is_none(),
            Self::F64(value) => value.is_none(),
            Self::Json(value) => value.is_none(),
        }
    }

    pub fn update_digest(&self, digest: &mut Sha256) {
        match self {
            Self::Text(value) => update_optional_bytes(digest, b't', value.as_deref()),
            Self::Bytes(value) => update_optional_bytes(digest, b'x', value.as_deref()),
            Self::Uuid(value) => {
                digest.update(b"u");
                match value {
                    Some(value) => {
                        digest.update(b"1");
                        digest.update(value.as_bytes());
                    }
                    None => digest.update(b"0"),
                }
            }
            Self::Timestamp(value) => {
                digest.update(b"d");
                match value {
                    Some(value) => {
                        digest.update(b"1");
                        digest.update(value.unix_timestamp_nanos().to_be_bytes());
                    }
                    None => digest.update(b"0"),
                }
            }
            Self::Bool(value) => {
                digest.update(b"b");
                digest.update(match value {
                    Some(true) => b"11",
                    Some(false) => b"10",
                    None => b"0-",
                });
            }
            Self::I16(value) => update_optional_number(digest, b's', *value),
            Self::I32(value) => update_optional_number(digest, b'i', *value),
            Self::I64(value) => update_optional_number(digest, b'l', *value),
            Self::F64(value) => {
                digest.update(b"f");
                match value {
                    Some(value) => {
                        digest.update(b"1");
                        digest.update(value.to_bits().to_be_bytes());
                    }
                    None => digest.update(b"0"),
                }
            }
            Self::Json(value) => {
                let canonical = value.as_ref().map(canonical_json);
                update_optional_bytes(digest, b'j', canonical.as_deref());
            }
        }
        digest.update(b"\0");
    }
}

pub fn parse_uuid(raw: &str) -> anyhow::Result<Uuid> {
    Uuid::parse_str(raw.trim()).context("invalid UUID")
}

pub fn parse_timestamp(raw: &str) -> anyhow::Result<OffsetDateTime> {
    let raw = raw.trim();
    let value = match OffsetDateTime::parse(raw, &Rfc3339) {
        Ok(value) => value,
        Err(_) => {
            let mut normalized = raw.replacen(' ', "T", 1);
            let time_and_zone = normalized.get(11..).unwrap_or_default();
            let has_explicit_zone = normalized.ends_with(['Z', 'z'])
                || time_and_zone.contains('+')
                || time_and_zone.contains('-');
            if !has_explicit_zone {
                normalized.push('Z');
            }
            OffsetDateTime::parse(&normalized, &Rfc3339).context("invalid SQLite timestamp")?
        }
    };
    // PostgreSQL stores timestamps at microsecond precision. Normalize the
    // SQLite value before it is bound or included in the source digest so the
    // value read back from PostgreSQL participates in the exact same digest.
    value
        .replace_nanosecond(value.nanosecond() / 1_000 * 1_000)
        .context("failed to normalize timestamp to PostgreSQL microseconds")
}

pub fn parse_bool(raw: i64) -> anyhow::Result<bool> {
    match raw {
        0 => Ok(false),
        1 => Ok(true),
        _ => anyhow::bail!("SQLite boolean must be exactly 0 or 1"),
    }
}

pub fn parse_json(raw: &str, shape: JsonShape) -> anyhow::Result<Value> {
    let value: Value = serde_json::from_str(raw).context("invalid JSON")?;
    let shape_matches = match shape {
        JsonShape::Any => true,
        JsonShape::Array => value.is_array(),
        JsonShape::Object => value.is_object(),
    };
    anyhow::ensure!(shape_matches, "JSON value has the wrong top-level shape");
    Ok(value)
}

fn canonical_json(value: &Value) -> Vec<u8> {
    let mut encoded = Vec::new();
    encode_canonical_json(value, &mut encoded);
    encoded
}

fn encode_canonical_json(value: &Value, encoded: &mut Vec<u8>) {
    match value {
        Value::Null => encoded.push(b'n'),
        Value::Bool(value) => encoded.extend_from_slice(if *value { b"b1" } else { b"b0" }),
        Value::Number(value) => {
            encoded.push(b'd');
            encode_length_prefixed(encoded, canonical_decimal(&value.to_string()).as_bytes());
        }
        Value::String(value) => {
            encoded.push(b's');
            encode_length_prefixed(encoded, value.as_bytes());
        }
        Value::Array(values) => {
            encoded.push(b'[');
            encoded.extend_from_slice(&(values.len() as u64).to_be_bytes());
            for value in values {
                encode_canonical_json(value, encoded);
            }
            encoded.push(b']');
        }
        Value::Object(values) => {
            encoded.push(b'{');
            encoded.extend_from_slice(&(values.len() as u64).to_be_bytes());
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            for (key, value) in entries {
                encode_length_prefixed(encoded, key.as_bytes());
                encode_canonical_json(value, encoded);
            }
            encoded.push(b'}');
        }
    }
}

fn encode_length_prefixed(encoded: &mut Vec<u8>, value: &[u8]) {
    encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
    encoded.extend_from_slice(value);
}

fn canonical_decimal(raw: &str) -> String {
    let (negative, unsigned) = raw
        .strip_prefix('-')
        .map_or((false, raw), |value| (true, value));
    let (mantissa, exponent) =
        unsigned
            .split_once(['e', 'E'])
            .map_or((unsigned, 0_i64), |(mantissa, exponent)| {
                (
                    mantissa,
                    exponent
                        .parse::<i64>()
                        .expect("serde_json emitted an invalid number exponent"),
                )
            });
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{integer}{fraction}");
    let mut power = exponent - fraction.len() as i64;
    let first_nonzero = digits
        .bytes()
        .position(|digit| digit != b'0')
        .unwrap_or(digits.len());
    digits.drain(..first_nonzero);
    if digits.is_empty() {
        return "0".to_owned();
    }
    let trailing_zeros = digits
        .bytes()
        .rev()
        .take_while(|digit| *digit == b'0')
        .count();
    digits.truncate(digits.len() - trailing_zeros);
    power += trailing_zeros as i64;
    format!("{}{digits}e{power}", if negative { "-" } else { "" })
}

fn update_optional_bytes<B>(digest: &mut Sha256, tag: u8, value: Option<B>)
where
    B: AsRef<[u8]>,
{
    digest.update([tag]);
    match value {
        Some(value) => {
            let value = value.as_ref();
            digest.update(b"1");
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        None => digest.update(b"0"),
    }
}

fn update_optional_number<T>(digest: &mut Sha256, tag: u8, value: Option<T>)
where
    T: ToString,
{
    let encoded = value.map(|value| value.to_string());
    update_optional_bytes(digest, tag, encoded.as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_accepts_hyphenated_and_compact_forms() {
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string()).unwrap(), id);
        assert_eq!(parse_uuid(&id.simple().to_string()).unwrap(), id);
        assert!(parse_uuid("not-a-uuid").is_err());
    }

    #[test]
    fn boolean_conversion_is_strict() {
        assert!(!parse_bool(0).unwrap());
        assert!(parse_bool(1).unwrap());
        assert!(parse_bool(-1).is_err());
        assert!(parse_bool(2).is_err());
    }

    #[test]
    fn timestamp_accepts_current_and_legacy_sqlite_forms() {
        let parsed = parse_timestamp("2026-08-08T01:02:03.456+02:00").unwrap();
        assert_eq!(parsed.unix_timestamp(), 1_786_143_723);
        let legacy = parse_timestamp("2026-08-08 01:02:03").unwrap();
        assert_eq!(legacy.offset(), time::UtcOffset::UTC);
        assert_eq!(
            parse_timestamp("2026-08-08 01:02:03-02:00").unwrap(),
            parse_timestamp("2026-08-08T03:02:03Z").unwrap()
        );
        assert!(parse_timestamp("not-a-timestamp").is_err());
    }

    #[test]
    fn timestamp_is_normalized_before_binding_and_digesting() {
        let normalized = parse_timestamp("2026-08-08T01:02:03.123456789Z").unwrap();
        let postgres_value = parse_timestamp("2026-08-08T01:02:03.123456Z").unwrap();
        assert_eq!(normalized, postgres_value);
        assert_eq!(normalized.nanosecond(), 123_456_000);

        let mut source_digest = Sha256::new();
        TypedValue::Timestamp(Some(normalized)).update_digest(&mut source_digest);
        let mut target_digest = Sha256::new();
        TypedValue::Timestamp(Some(postgres_value)).update_digest(&mut target_digest);
        assert_eq!(source_digest.finalize(), target_digest.finalize());
    }

    #[test]
    fn json_shape_validation_rejects_schema_mismatches() {
        assert!(parse_json("[]", JsonShape::Array).is_ok());
        assert!(parse_json("{}", JsonShape::Object).is_ok());
        assert!(parse_json("{}", JsonShape::Array).is_err());
        assert!(parse_json("not-json", JsonShape::Any).is_err());
    }

    #[test]
    fn canonical_digest_is_stable_and_type_aware() {
        let mut first = Sha256::new();
        TypedValue::Json(Some(serde_json::json!({"b": 2, "a": 1}))).update_digest(&mut first);
        let mut second = Sha256::new();
        TypedValue::Json(Some(serde_json::json!({"a": 1, "b": 2}))).update_digest(&mut second);
        assert_eq!(first.finalize(), second.finalize());

        let mut text = Sha256::new();
        TypedValue::Text(Some("1".to_owned())).update_digest(&mut text);
        let mut number = Sha256::new();
        TypedValue::I64(Some(1)).update_digest(&mut number);
        assert_ne!(text.finalize(), number.finalize());
    }

    #[test]
    fn binary_digest_is_exact_and_type_aware() {
        let digest = |value: TypedValue| {
            let mut digest = Sha256::new();
            value.update_digest(&mut digest);
            digest.finalize()
        };
        assert_eq!(
            digest(TypedValue::Bytes(Some(vec![0x00, 0xff, 0x80]))),
            digest(TypedValue::Bytes(Some(vec![0x00, 0xff, 0x80])))
        );
        assert_ne!(
            digest(TypedValue::Bytes(Some(vec![0x00, 0xff, 0x80]))),
            digest(TypedValue::Bytes(Some(vec![0x00, 0xff, 0x81])))
        );
        assert_ne!(
            digest(TypedValue::Bytes(Some(vec![0x31]))),
            digest(TypedValue::Text(Some("1".to_owned())))
        );
    }

    #[test]
    fn canonical_json_normalizes_object_order_and_decimal_spelling() {
        let variants = [
            serde_json::from_str::<Value>(r#"{"b": 1e2, "a": 1.2300}"#).unwrap(),
            serde_json::from_str::<Value>(r#"{"a": 1.23, "b": 100.00}"#).unwrap(),
        ];
        assert_eq!(canonical_json(&variants[0]), canonical_json(&variants[1]));
        assert_eq!(canonical_decimal("-0.000"), "0");
        assert_eq!(canonical_decimal("12300"), "123e2");
        assert_eq!(canonical_decimal("0.0012300"), "123e-5");
    }
}
