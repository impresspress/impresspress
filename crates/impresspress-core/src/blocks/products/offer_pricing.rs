//! Typed, deterministic pricing for commerce-v2 offers.
//!
//! New offers use validated inputs, explicit conditions, and integer
//! minor-unit amounts. This is the only product pricing engine.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use chrono::{NaiveDate, NaiveDateTime};
use serde_json::Value;

use super::{
    contracts::{
        AmountRule, BillingScheme, Condition, MoneyBreakdown, Offer, OfferMode, PackageRounding,
        PricingPreview, PricingPreviewRequest, PricingTier, QuantityRule, RecurringInterval,
        ResolvedComponent, VariableDefinition, VariableKind, COMMERCE_SCHEMA_VERSION,
    },
    money::normalize_currency,
};

const MAX_CONDITION_DEPTH: usize = 32;
const MAX_ORDER_QUANTITY: u64 = 1_000_000;
const MAX_DECIMAL_SCALE: u32 = 9;
const DEFAULT_MAX_TEXT_LENGTH: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingError {
    pub code: &'static str,
    pub field: Option<String>,
    pub message: String,
}

impl PricingError {
    fn new(code: &'static str, field: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code,
            field: field.map(str::to_string),
            message: message.into(),
        }
    }
}

impl fmt::Display for PricingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.field {
            Some(field) => write!(formatter, "{field}: {}", self.message),
            None => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for PricingError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Decimal {
    coefficient: i128,
    scale: u32,
}

impl Decimal {
    fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        let (negative, unsigned) = if let Some(value) = input.strip_prefix('-') {
            (true, value)
        } else {
            (false, input.strip_prefix('+').unwrap_or(input))
        };
        let mut parts = unsigned.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || (whole.is_empty() && fraction.is_empty())
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("must be a plain decimal number".to_string());
        }
        if fraction.len() > MAX_DECIMAL_SCALE as usize {
            return Err(format!(
                "must have at most {MAX_DECIMAL_SCALE} decimal places"
            ));
        }
        let digits = format!("{}{}", if whole.is_empty() { "0" } else { whole }, fraction);
        let mut coefficient = digits
            .parse::<i128>()
            .map_err(|_| "number is too large".to_string())?;
        if negative {
            coefficient = -coefficient;
        }
        let mut value = Self {
            coefficient,
            scale: fraction.len() as u32,
        };
        while value.scale > 0 && value.coefficient % 10 == 0 {
            value.coefficient /= 10;
            value.scale -= 1;
        }
        Ok(value)
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        match value {
            Value::Number(value) => Self::parse(&value.to_string()),
            Value::String(value) => Self::parse(value),
            _ => Err("must be a number or decimal string".to_string()),
        }
    }

    fn aligned(self, scale: u32) -> Result<i128, String> {
        let multiplier = 10_i128
            .checked_pow(scale.saturating_sub(self.scale))
            .ok_or_else(|| "number is too large".to_string())?;
        self.coefficient
            .checked_mul(multiplier)
            .ok_or_else(|| "number is too large".to_string())
    }

    fn compare(self, other: Self) -> Result<Ordering, String> {
        let scale = self.scale.max(other.scale);
        Ok(self.aligned(scale)?.cmp(&other.aligned(scale)?))
    }

    fn is_step_from(self, base: Self, step: Self) -> Result<bool, String> {
        if step.coefficient <= 0 {
            return Err("step must be greater than zero".to_string());
        }
        let scale = self.scale.max(base.scale).max(step.scale);
        let difference = self
            .aligned(scale)?
            .checked_sub(base.aligned(scale)?)
            .ok_or_else(|| "number is too large".to_string())?;
        Ok(difference % step.aligned(scale)? == 0)
    }

    fn multiply_minor(self, amount_minor: i64) -> Result<i64, String> {
        let numerator = self
            .coefficient
            .checked_mul(amount_minor as i128)
            .ok_or_else(|| "calculated amount is too large".to_string())?;
        let denominator = 10_i128
            .checked_pow(self.scale)
            .ok_or_else(|| "number is too large".to_string())?;
        if numerator % denominator != 0 {
            return Err(
                "value does not resolve to a whole minor-unit amount; adjust the value or rate"
                    .to_string(),
            );
        }
        i64::try_from(numerator / denominator)
            .map_err(|_| "calculated amount is too large".to_string())
    }

    fn as_u64(self) -> Option<u64> {
        let denominator = 10_i128.checked_pow(self.scale)?;
        if self.coefficient < 0 || self.coefficient % denominator != 0 {
            return None;
        }
        u64::try_from(self.coefficient / denominator).ok()
    }

    fn canonical(self) -> String {
        let sign = if self.coefficient < 0 { "-" } else { "" };
        let digits = self.coefficient.unsigned_abs().to_string();
        if self.scale == 0 {
            return format!("{sign}{digits}");
        }
        let scale = self.scale as usize;
        let padded = if digits.len() <= scale {
            format!("{}{}", "0".repeat(scale + 1 - digits.len()), digits)
        } else {
            digits
        };
        let split = padded.len() - scale;
        format!("{sign}{}.{}", &padded[..split], &padded[split..])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ValidatedValue {
    Number(Decimal),
    Integer(i64),
    Boolean(bool),
    Date(NaiveDate),
    DateTime(NaiveDateTime),
    Select(String),
    MultiSelect(Vec<String>),
    Text(String),
}

impl ValidatedValue {
    fn normalized_json(&self) -> Value {
        match self {
            Self::Number(value) => Value::String(value.canonical()),
            Self::Integer(value) => Value::Number((*value).into()),
            Self::Boolean(value) => Value::Bool(*value),
            Self::Date(value) => Value::String(value.format("%Y-%m-%d").to_string()),
            Self::DateTime(value) => Value::String(value.format("%Y-%m-%dT%H:%M").to_string()),
            Self::Select(value) | Self::Text(value) => Value::String(value.clone()),
            Self::MultiSelect(values) => {
                Value::Array(values.iter().cloned().map(Value::String).collect())
            }
        }
    }

    fn decimal(&self) -> Option<Decimal> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Integer(value) => Some(Decimal {
                coefficient: *value as i128,
                scale: 0,
            }),
            _ => None,
        }
    }

    fn scalar(&self) -> Option<&str> {
        match self {
            Self::Select(value) | Self::Text(value) => Some(value),
            _ => None,
        }
    }

    fn equals_json(&self, expected: &Value) -> Result<bool, String> {
        match self {
            Self::Number(actual) => {
                Ok(actual.compare(Decimal::from_json(expected)?)? == Ordering::Equal)
            }
            Self::Integer(actual) => expected
                .as_i64()
                .map(|expected| actual == &expected)
                .ok_or_else(|| "comparison value must be an integer".to_string()),
            Self::Boolean(actual) => expected
                .as_bool()
                .map(|expected| actual == &expected)
                .ok_or_else(|| "comparison value must be a boolean".to_string()),
            Self::Date(actual) => expected
                .as_str()
                .ok_or_else(|| "comparison value must be a date".to_string())
                .and_then(|expected| {
                    NaiveDate::parse_from_str(expected, "%Y-%m-%d")
                        .map(|expected| actual == &expected)
                        .map_err(|_| "comparison value must use YYYY-MM-DD".to_string())
                }),
            Self::DateTime(actual) => expected
                .as_str()
                .ok_or_else(|| "comparison value must be a date and time".to_string())
                .and_then(|expected| {
                    NaiveDateTime::parse_from_str(expected, "%Y-%m-%dT%H:%M")
                        .map(|expected| actual == &expected)
                        .map_err(|_| "comparison value must use YYYY-MM-DDTHH:MM".to_string())
                }),
            Self::Select(actual) | Self::Text(actual) => expected
                .as_str()
                .map(|expected| actual == expected)
                .ok_or_else(|| "comparison value must be text".to_string()),
            Self::MultiSelect(actual) => {
                let expected = expected
                    .as_array()
                    .ok_or_else(|| "comparison value must be a text array".to_string())?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_string)
                            .ok_or_else(|| "comparison value must be a text array".to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(actual == &expected)
            }
        }
    }

    fn ordered_json(&self, expected: &Value) -> Result<Ordering, String> {
        match self {
            Self::Number(actual) => actual.compare(Decimal::from_json(expected)?),
            Self::Integer(actual) => expected
                .as_i64()
                .map(|expected| actual.cmp(&expected))
                .ok_or_else(|| "comparison value must be an integer".to_string()),
            Self::Date(actual) => expected
                .as_str()
                .ok_or_else(|| "comparison value must be a date".to_string())
                .and_then(|expected| {
                    NaiveDate::parse_from_str(expected, "%Y-%m-%d")
                        .map(|expected| actual.cmp(&expected))
                        .map_err(|_| "comparison value must use YYYY-MM-DD".to_string())
                }),
            Self::DateTime(actual) => expected
                .as_str()
                .ok_or_else(|| "comparison value must be a date and time".to_string())
                .and_then(|expected| {
                    NaiveDateTime::parse_from_str(expected, "%Y-%m-%dT%H:%M")
                        .map(|expected| actual.cmp(&expected))
                        .map_err(|_| "comparison value must use YYYY-MM-DDTHH:MM".to_string())
                }),
            _ => Err("ordered comparison requires a number, date, or date and time".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ValidatedInputs {
    values: BTreeMap<String, ValidatedValue>,
}

impl ValidatedInputs {
    pub fn normalized(&self) -> BTreeMap<String, Value> {
        self.values
            .iter()
            .map(|(key, value)| (key.clone(), value.normalized_json()))
            .collect()
    }
}

fn invalid_input(definition: &VariableDefinition, message: impl Into<String>) -> PricingError {
    PricingError::new("invalid_input", Some(&definition.key), message)
}

fn validate_bounds(definition: &VariableDefinition, value: Decimal) -> Result<(), PricingError> {
    let bad_definition = |message: String| {
        PricingError::new(
            "invalid_offer",
            Some(&definition.key),
            format!("invalid variable definition: {message}"),
        )
    };
    let minimum = definition
        .minimum
        .as_deref()
        .map(Decimal::parse)
        .transpose()
        .map_err(bad_definition)?;
    let maximum = definition
        .maximum
        .as_deref()
        .map(Decimal::parse)
        .transpose()
        .map_err(bad_definition)?;
    if minimum.is_some_and(|minimum| value.compare(minimum).ok() == Some(Ordering::Less)) {
        return Err(invalid_input(definition, "is below the minimum"));
    }
    if maximum.is_some_and(|maximum| value.compare(maximum).ok() == Some(Ordering::Greater)) {
        return Err(invalid_input(definition, "is above the maximum"));
    }
    if let Some(step) = definition.step.as_deref() {
        let step = Decimal::parse(step).map_err(bad_definition)?;
        let base = minimum.unwrap_or(Decimal {
            coefficient: 0,
            scale: 0,
        });
        if !value.is_step_from(base, step).map_err(bad_definition)? {
            return Err(invalid_input(
                definition,
                "does not align to the configured step",
            ));
        }
    }
    Ok(())
}

fn validate_date_bounds(
    definition: &VariableDefinition,
    value: NaiveDate,
) -> Result<(), PricingError> {
    let parse = |raw: &str| {
        NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
            PricingError::new(
                "invalid_offer",
                Some(&definition.key),
                "date bounds must use YYYY-MM-DD",
            )
        })
    };
    if definition
        .minimum
        .as_deref()
        .map(parse)
        .transpose()?
        .is_some_and(|minimum| value < minimum)
    {
        return Err(invalid_input(definition, "is before the minimum date"));
    }
    if definition
        .maximum
        .as_deref()
        .map(parse)
        .transpose()?
        .is_some_and(|maximum| value > maximum)
    {
        return Err(invalid_input(definition, "is after the maximum date"));
    }
    Ok(())
}

fn validate_date_time_bounds(
    definition: &VariableDefinition,
    value: NaiveDateTime,
) -> Result<(), PricingError> {
    let parse = |raw: &str| {
        NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M").map_err(|_| {
            PricingError::new(
                "invalid_offer",
                Some(&definition.key),
                "date-time bounds must use YYYY-MM-DDTHH:MM",
            )
        })
    };
    if definition
        .minimum
        .as_deref()
        .map(parse)
        .transpose()?
        .is_some_and(|minimum| value < minimum)
    {
        return Err(invalid_input(
            definition,
            "is before the minimum date and time",
        ));
    }
    if definition
        .maximum
        .as_deref()
        .map(parse)
        .transpose()?
        .is_some_and(|maximum| value > maximum)
    {
        return Err(invalid_input(
            definition,
            "is after the maximum date and time",
        ));
    }
    Ok(())
}

fn parse_input(
    definition: &VariableDefinition,
    value: &Value,
) -> Result<ValidatedValue, PricingError> {
    match definition.kind {
        VariableKind::Number => {
            let value =
                Decimal::from_json(value).map_err(|message| invalid_input(definition, message))?;
            validate_bounds(definition, value)?;
            Ok(ValidatedValue::Number(value))
        }
        VariableKind::Integer => {
            let value = value
                .as_i64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                .ok_or_else(|| invalid_input(definition, "must be an integer"))?;
            validate_bounds(
                definition,
                Decimal {
                    coefficient: value as i128,
                    scale: 0,
                },
            )?;
            Ok(ValidatedValue::Integer(value))
        }
        VariableKind::Boolean => value
            .as_bool()
            .map(ValidatedValue::Boolean)
            .ok_or_else(|| invalid_input(definition, "must be a boolean")),
        VariableKind::Date => {
            let raw = value
                .as_str()
                .ok_or_else(|| invalid_input(definition, "must be a date"))?;
            let value = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .map_err(|_| invalid_input(definition, "must use YYYY-MM-DD"))?;
            validate_date_bounds(definition, value)?;
            Ok(ValidatedValue::Date(value))
        }
        VariableKind::DateTime => {
            let raw = value
                .as_str()
                .ok_or_else(|| invalid_input(definition, "must be a date and time"))?;
            let value = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M")
                .map_err(|_| invalid_input(definition, "must use YYYY-MM-DDTHH:MM"))?;
            validate_date_time_bounds(definition, value)?;
            Ok(ValidatedValue::DateTime(value))
        }
        VariableKind::Select => {
            let value = value
                .as_str()
                .ok_or_else(|| invalid_input(definition, "must be one text option"))?;
            if !definition
                .allowed_values
                .iter()
                .any(|allowed| allowed == value)
            {
                return Err(invalid_input(definition, "is not an allowed option"));
            }
            Ok(ValidatedValue::Select(value.to_string()))
        }
        VariableKind::MultiSelect => {
            let values = value
                .as_array()
                .ok_or_else(|| invalid_input(definition, "must be an array of options"))?;
            let mut parsed = Vec::with_capacity(values.len());
            let mut unique = BTreeSet::new();
            for value in values {
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid_input(definition, "must contain text options"))?;
                if !definition
                    .allowed_values
                    .iter()
                    .any(|allowed| allowed == value)
                {
                    return Err(invalid_input(
                        definition,
                        format!("{value:?} is not an allowed option"),
                    ));
                }
                if !unique.insert(value.to_string()) {
                    return Err(invalid_input(
                        definition,
                        format!("{value:?} is duplicated"),
                    ));
                }
                parsed.push(value.to_string());
            }
            Ok(ValidatedValue::MultiSelect(parsed))
        }
        VariableKind::Text => {
            let value = value
                .as_str()
                .ok_or_else(|| invalid_input(definition, "must be text"))?;
            let maximum = definition
                .maximum_length
                .unwrap_or(DEFAULT_MAX_TEXT_LENGTH)
                .min(DEFAULT_MAX_TEXT_LENGTH);
            if value.chars().count() > maximum {
                return Err(invalid_input(
                    definition,
                    format!("must be at most {maximum} characters"),
                ));
            }
            Ok(ValidatedValue::Text(value.to_string()))
        }
    }
}

pub fn validate_inputs(
    definitions: &[VariableDefinition],
    raw: &BTreeMap<String, Value>,
) -> Result<ValidatedInputs, PricingError> {
    let mut by_key = BTreeMap::new();
    for definition in definitions {
        if definition.key.is_empty()
            || !definition
                .key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(PricingError::new(
                "invalid_offer",
                Some(&definition.key),
                "variable key may contain only letters, numbers, and underscores",
            ));
        }
        if by_key.insert(definition.key.as_str(), definition).is_some() {
            return Err(PricingError::new(
                "invalid_offer",
                Some(&definition.key),
                "variable key is duplicated",
            ));
        }
        if matches!(
            definition.kind,
            VariableKind::Select | VariableKind::MultiSelect
        ) && definition.allowed_values.is_empty()
        {
            return Err(PricingError::new(
                "invalid_offer",
                Some(&definition.key),
                "select variables require allowed values",
            ));
        }
    }
    for key in raw.keys() {
        if !by_key.contains_key(key.as_str()) {
            return Err(PricingError::new(
                "unknown_input",
                Some(key),
                "input is not defined by the offer",
            ));
        }
    }
    let mut values = BTreeMap::new();
    for definition in definitions {
        match raw
            .get(&definition.key)
            .or(definition.default_value.as_ref())
        {
            Some(value) => {
                values.insert(definition.key.clone(), parse_input(definition, value)?);
            }
            None if definition.required => {
                return Err(PricingError::new(
                    "missing_input",
                    Some(&definition.key),
                    "input is required",
                ));
            }
            None => {}
        }
    }
    Ok(ValidatedInputs { values })
}

fn input<'a>(inputs: &'a ValidatedInputs, key: &str) -> Result<&'a ValidatedValue, PricingError> {
    inputs.values.get(key).ok_or_else(|| {
        PricingError::new(
            "condition_input_missing",
            Some(key),
            "condition references an input with no value",
        )
    })
}

fn ordered_comparison(
    inputs: &ValidatedInputs,
    key: &str,
    expected: &Value,
) -> Result<Ordering, PricingError> {
    input(inputs, key)?
        .ordered_json(expected)
        .map_err(|message| PricingError::new("invalid_condition", Some(key), message))
}

fn evaluate_condition_at(
    condition: &Condition,
    inputs: &ValidatedInputs,
    depth: usize,
) -> Result<bool, PricingError> {
    if depth > MAX_CONDITION_DEPTH {
        return Err(PricingError::new(
            "invalid_condition",
            None,
            "condition tree is too deeply nested",
        ));
    }
    match condition {
        Condition::Always => Ok(true),
        Condition::All { conditions } => {
            for condition in conditions {
                if !evaluate_condition_at(condition, inputs, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Condition::Any { conditions } => {
            for condition in conditions {
                if evaluate_condition_at(condition, inputs, depth + 1)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Condition::Not { condition } => Ok(!evaluate_condition_at(condition, inputs, depth + 1)?),
        Condition::Present { input: key } => Ok(inputs.values.contains_key(key)),
        Condition::Equals { input: key, value } | Condition::NotEquals { input: key, value } => {
            let equals = input(inputs, key)?
                .equals_json(value)
                .map_err(|message| PricingError::new("invalid_condition", Some(key), message))?;
            Ok(if matches!(condition, Condition::NotEquals { .. }) {
                !equals
            } else {
                equals
            })
        }
        Condition::GreaterThan { input: key, value } => {
            Ok(ordered_comparison(inputs, key, value)? == Ordering::Greater)
        }
        Condition::GreaterThanOrEqual { input: key, value } => Ok(matches!(
            ordered_comparison(inputs, key, value)?,
            Ordering::Greater | Ordering::Equal
        )),
        Condition::LessThan { input: key, value } => {
            Ok(ordered_comparison(inputs, key, value)? == Ordering::Less)
        }
        Condition::LessThanOrEqual { input: key, value } => Ok(matches!(
            ordered_comparison(inputs, key, value)?,
            Ordering::Less | Ordering::Equal
        )),
        Condition::In { input: key, values } => {
            for value in values {
                if input(inputs, key)?
                    .equals_json(value)
                    .map_err(|message| PricingError::new("invalid_condition", Some(key), message))?
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Condition::Contains { input: key, value } => {
            let expected = value.as_str().ok_or_else(|| {
                PricingError::new(
                    "invalid_condition",
                    Some(key),
                    "contains value must be text",
                )
            })?;
            match input(inputs, key)? {
                ValidatedValue::MultiSelect(values) => {
                    Ok(values.iter().any(|value| value == expected))
                }
                ValidatedValue::Text(value) => Ok(value.contains(expected)),
                _ => Err(PricingError::new(
                    "invalid_condition",
                    Some(key),
                    "contains requires text or multi-select input",
                )),
            }
        }
    }
}

pub fn evaluate_condition(
    condition: &Condition,
    inputs: &ValidatedInputs,
) -> Result<bool, PricingError> {
    evaluate_condition_at(condition, inputs, 0)
}

fn decimal_input(inputs: &ValidatedInputs, key: &str) -> Result<Decimal, PricingError> {
    input(inputs, key)?.decimal().ok_or_else(|| {
        PricingError::new(
            "invalid_offer",
            Some(key),
            "amount rule requires a numeric input",
        )
    })
}

fn whole_input(inputs: &ValidatedInputs, key: &str, model: &str) -> Result<u64, PricingError> {
    decimal_input(inputs, key)?.as_u64().ok_or_else(|| {
        PricingError::new(
            "invalid_input",
            Some(key),
            format!("{model} pricing requires a non-negative whole number"),
        )
    })
}

fn checked_tier_amount(
    units: u64,
    unit_amount_minor: i64,
    flat_amount_minor: i64,
) -> Result<i128, String> {
    (units as i128)
        .checked_mul(unit_amount_minor as i128)
        .and_then(|amount| amount.checked_add(flat_amount_minor as i128))
        .ok_or_else(|| "calculated amount is too large".to_string())
}

fn tiered_amount(
    inputs: &ValidatedInputs,
    key: &str,
    tiers: &[PricingTier],
    graduated: bool,
) -> Result<i64, PricingError> {
    let units = whole_input(inputs, key, if graduated { "graduated" } else { "volume" })?;
    let amount = if graduated {
        let mut previous = 0_u64;
        let mut total = 0_i128;
        for tier in tiers {
            let upper = tier.up_to.unwrap_or(u64::MAX);
            let tier_units = units.min(upper).saturating_sub(previous);
            if tier_units > 0 {
                total = total
                    .checked_add(
                        checked_tier_amount(
                            tier_units,
                            tier.unit_amount_minor,
                            tier.flat_amount_minor,
                        )
                        .map_err(|message| {
                            PricingError::new("amount_overflow", Some(key), message)
                        })?,
                    )
                    .ok_or_else(|| {
                        PricingError::new(
                            "amount_overflow",
                            Some(key),
                            "calculated amount is too large",
                        )
                    })?;
            }
            if units <= upper {
                break;
            }
            previous = upper;
        }
        total
    } else {
        let tier = tiers
            .iter()
            .find(|tier| tier.up_to.is_none_or(|upper| units <= upper))
            .ok_or_else(|| {
                PricingError::new(
                    "invalid_offer",
                    Some(key),
                    "volume tiers do not cover this input",
                )
            })?;
        checked_tier_amount(units, tier.unit_amount_minor, tier.flat_amount_minor)
            .map_err(|message| PricingError::new("amount_overflow", Some(key), message))?
    };
    i64::try_from(amount).map_err(|_| {
        PricingError::new(
            "amount_overflow",
            Some(key),
            "calculated amount is too large",
        )
    })
}

fn resolve_amount(rule: &AmountRule, inputs: &ValidatedInputs) -> Result<i64, PricingError> {
    let amount = match rule {
        AmountRule::Fixed { unit_amount_minor } => *unit_amount_minor,
        AmountRule::PerUnit {
            input: key,
            unit_amount_minor,
        } => decimal_input(inputs, key)?
            .multiply_minor(*unit_amount_minor)
            .map_err(|message| PricingError::new("invalid_input", Some(key), message))?,
        AmountRule::FlatPlusPerUnit {
            base_amount_minor,
            input: key,
            unit_amount_minor,
        } => decimal_input(inputs, key)?
            .multiply_minor(*unit_amount_minor)
            .and_then(|value| {
                value
                    .checked_add(*base_amount_minor)
                    .ok_or_else(|| "calculated amount is too large".to_string())
            })
            .map_err(|message| PricingError::new("invalid_input", Some(key), message))?,
        AmountRule::Lookup { input: key, prices } => {
            let selected = input(inputs, key)?.scalar().ok_or_else(|| {
                PricingError::new(
                    "invalid_offer",
                    Some(key),
                    "lookup amount requires select or text input",
                )
            })?;
            *prices.get(selected).ok_or_else(|| {
                PricingError::new(
                    "invalid_input",
                    Some(key),
                    "selected value has no configured price",
                )
            })?
        }
        AmountRule::Graduated { input: key, tiers } => tiered_amount(inputs, key, tiers, true)?,
        AmountRule::Volume { input: key, tiers } => tiered_amount(inputs, key, tiers, false)?,
        AmountRule::Package {
            input: key,
            units_per_package,
            package_amount_minor,
            rounding,
        } => {
            let units = whole_input(inputs, key, "package")?;
            let packages = match rounding {
                PackageRounding::Up => units
                    .checked_add(units_per_package.saturating_sub(1))
                    .and_then(|value| value.checked_div(*units_per_package)),
                PackageRounding::Exact if units % units_per_package == 0 => {
                    Some(units / units_per_package)
                }
                PackageRounding::Exact => {
                    return Err(PricingError::new(
                        "invalid_input",
                        Some(key),
                        "input must be an exact multiple of the package size",
                    ));
                }
            }
            .ok_or_else(|| {
                PricingError::new(
                    "amount_overflow",
                    Some(key),
                    "package quantity is too large",
                )
            })?;
            i64::try_from(
                (packages as i128)
                    .checked_mul(*package_amount_minor as i128)
                    .ok_or_else(|| {
                        PricingError::new(
                            "amount_overflow",
                            Some(key),
                            "calculated amount is too large",
                        )
                    })?,
            )
            .map_err(|_| {
                PricingError::new(
                    "amount_overflow",
                    Some(key),
                    "calculated amount is too large",
                )
            })?
        }
    };
    if amount < 0 {
        return Err(PricingError::new(
            "invalid_offer",
            None,
            "component amounts must not be negative",
        ));
    }
    Ok(amount)
}

fn resolve_quantity(rule: &QuantityRule, inputs: &ValidatedInputs) -> Result<u64, PricingError> {
    match rule {
        QuantityRule::Fixed { value } if *value > 0 => Ok(*value),
        QuantityRule::Fixed { .. } => Err(PricingError::new(
            "invalid_offer",
            None,
            "component quantity must be greater than zero",
        )),
        QuantityRule::FromInput {
            input: key,
            minimum,
            maximum,
        } => {
            let quantity = decimal_input(inputs, key)?.as_u64().ok_or_else(|| {
                PricingError::new(
                    "invalid_input",
                    Some(key),
                    "quantity must be a non-negative whole number",
                )
            })?;
            if quantity < *minimum || maximum.is_some_and(|maximum| quantity > maximum) {
                return Err(PricingError::new(
                    "invalid_input",
                    Some(key),
                    "quantity is outside the component bounds",
                ));
            }
            Ok(quantity)
        }
    }
}

fn condition_keys_exist(condition: &Condition, keys: &BTreeSet<&str>) -> bool {
    match condition {
        Condition::Always => true,
        Condition::All { conditions } | Condition::Any { conditions } => conditions
            .iter()
            .all(|condition| condition_keys_exist(condition, keys)),
        Condition::Not { condition } => condition_keys_exist(condition, keys),
        Condition::Present { input }
        | Condition::Equals { input, .. }
        | Condition::NotEquals { input, .. }
        | Condition::GreaterThan { input, .. }
        | Condition::GreaterThanOrEqual { input, .. }
        | Condition::LessThan { input, .. }
        | Condition::LessThanOrEqual { input, .. }
        | Condition::In { input, .. }
        | Condition::Contains { input, .. } => keys.contains(input.as_str()),
    }
}

fn validate_tiers(tiers: &[PricingTier]) -> Result<(), &'static str> {
    if tiers.is_empty() {
        return Err("tiers must not be empty");
    }
    let mut previous = 0_u64;
    for (index, tier) in tiers.iter().enumerate() {
        if tier.unit_amount_minor < 0 || tier.flat_amount_minor < 0 {
            return Err("tier amounts must not be negative");
        }
        let is_last = index + 1 == tiers.len();
        match tier.up_to {
            Some(upper) if upper > previous && !is_last => previous = upper,
            Some(_) if is_last => return Err("the final tier must be open ended"),
            Some(_) => return Err("tier bounds must be strictly increasing"),
            None if is_last => {}
            None => return Err("only the final tier may be open ended"),
        }
    }
    Ok(())
}

fn recurrence_matches(
    mode: OfferMode,
    interval: Option<RecurringInterval>,
    interval_count: u32,
    component: &super::contracts::OfferComponent,
) -> bool {
    match (mode, &component.recurrence) {
        (OfferMode::Payment, Some(_)) => false,
        (OfferMode::Subscription, Some(recurrence)) => {
            Some(recurrence.interval) == interval && recurrence.interval_count == interval_count
        }
        _ => true,
    }
}

fn validate_checkout_policy(offer: &Offer) -> Result<(), PricingError> {
    let policy = &offer.checkout;
    if policy.minimum_total_minor.is_some_and(|amount| amount < 0)
        || policy.maximum_total_minor.is_some_and(|amount| amount <= 0)
    {
        return Err(PricingError::new(
            "invalid_offer",
            Some("checkout.minimum_total_minor"),
            "minimum total must not be negative and maximum total must be greater than zero",
        ));
    }
    if matches!(
        (policy.minimum_total_minor, policy.maximum_total_minor),
        (Some(minimum), Some(maximum)) if minimum > maximum
    ) {
        return Err(PricingError::new(
            "invalid_offer",
            Some("checkout.maximum_total_minor"),
            "maximum total must be greater than or equal to the minimum total",
        ));
    }
    if policy.trial_days > 730 {
        return Err(PricingError::new(
            "invalid_offer",
            Some("checkout.trial_days"),
            "trial days must not exceed 730",
        ));
    }
    if (!policy.allowed_shipping_countries.is_empty() || !policy.shipping_options.is_empty())
        && !policy.collect_shipping_address
    {
        return Err(PricingError::new(
            "invalid_offer",
            Some("checkout.collect_shipping_address"),
            "shipping countries and rates require shipping-address collection",
        ));
    }
    if policy.allowed_shipping_countries.len() > 50 {
        return Err(PricingError::new(
            "invalid_offer",
            Some("checkout.allowed_shipping_countries"),
            "at most 50 shipping countries may be configured",
        ));
    }
    let mut countries = BTreeSet::new();
    for country in &policy.allowed_shipping_countries {
        let country = country.trim();
        if country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return Err(PricingError::new(
                "invalid_offer",
                Some("checkout.allowed_shipping_countries"),
                "shipping countries must be two-letter country codes",
            ));
        }
        if !countries.insert(country.to_ascii_uppercase()) {
            return Err(PricingError::new(
                "invalid_offer",
                Some("checkout.allowed_shipping_countries"),
                "shipping countries must be unique",
            ));
        }
    }
    if policy.shipping_options.len() > 5 {
        return Err(PricingError::new(
            "invalid_offer",
            Some("checkout.shipping_options"),
            "Stripe Checkout supports at most five shipping options",
        ));
    }
    for (index, option) in policy.shipping_options.iter().enumerate() {
        let field = format!("checkout.shipping_options[{index}]");
        let display_name = option.display_name.trim();
        if display_name.is_empty() || display_name.chars().count() > 100 {
            return Err(PricingError::new(
                "invalid_offer",
                Some(&field),
                "shipping option names must contain between 1 and 100 characters",
            ));
        }
        if option.amount_minor < 0 {
            return Err(PricingError::new(
                "invalid_offer",
                Some(&field),
                "shipping amounts must not be negative",
            ));
        }
        let stripe_id = option.stripe_shipping_rate_id.trim();
        if !stripe_id.is_empty()
            && (!stripe_id.starts_with("shr_")
                || !stripe_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        {
            return Err(PricingError::new(
                "invalid_offer",
                Some(&field),
                "Stripe shipping rate IDs must start with shr_",
            ));
        }
        if let Some(estimate) = &option.delivery_estimate {
            if estimate.minimum.is_none() && estimate.maximum.is_none() {
                return Err(PricingError::new(
                    "invalid_offer",
                    Some(&field),
                    "a delivery estimate needs a minimum or maximum",
                ));
            }
            if estimate.minimum == Some(0)
                || estimate.maximum == Some(0)
                || matches!((estimate.minimum, estimate.maximum), (Some(min), Some(max)) if min > max)
            {
                return Err(PricingError::new(
                    "invalid_offer",
                    Some(&field),
                    "delivery estimates must be positive and ordered",
                ));
            }
        }
    }
    Ok(())
}

pub fn validate_offer(offer: &Offer) -> Result<(), PricingError> {
    normalize_currency(&offer.currency)
        .map_err(|message| PricingError::new("invalid_offer", Some("currency"), message))?;
    if offer.version == 0 || offer.interval_count == 0 || offer.components.is_empty() {
        return Err(PricingError::new(
            "invalid_offer",
            None,
            "offer requires a version, interval count, and at least one component",
        ));
    }
    if matches!(offer.mode, OfferMode::Payment) && offer.recurring_interval.is_some()
        || matches!(offer.mode, OfferMode::Subscription) && offer.recurring_interval.is_none()
    {
        return Err(PricingError::new(
            "invalid_offer",
            Some("recurring_interval"),
            "recurrence does not match the offer mode",
        ));
    }
    validate_checkout_policy(offer)?;
    let keys: BTreeSet<_> = offer
        .variables
        .iter()
        .map(|variable| variable.key.as_str())
        .collect();
    if keys.len() != offer.variables.len() {
        return Err(PricingError::new(
            "invalid_offer",
            Some("variables"),
            "variable keys must be unique",
        ));
    }
    let mut component_keys = BTreeSet::new();
    let mut has_tiered_amount = false;
    for component in &offer.components {
        if !component_keys.insert(component.key.as_str())
            || !condition_keys_exist(&component.condition, &keys)
            || !recurrence_matches(
                offer.mode,
                offer.recurring_interval,
                offer.interval_count,
                component,
            )
        {
            return Err(PricingError::new(
                "invalid_offer",
                Some(&component.key),
                "component key, condition, or recurrence is invalid",
            ));
        }
        let amount_key = match &component.amount {
            AmountRule::Fixed { unit_amount_minor } => {
                if *unit_amount_minor < 0 {
                    return Err(PricingError::new(
                        "invalid_offer",
                        Some(&component.key),
                        "component amount must not be negative",
                    ));
                }
                None
            }
            AmountRule::PerUnit {
                input,
                unit_amount_minor,
            } => {
                if *unit_amount_minor < 0 {
                    return Err(PricingError::new(
                        "invalid_offer",
                        Some(&component.key),
                        "component amount must not be negative",
                    ));
                }
                Some(input)
            }
            AmountRule::FlatPlusPerUnit {
                base_amount_minor,
                input,
                unit_amount_minor,
            } => {
                if *base_amount_minor < 0 || *unit_amount_minor < 0 {
                    return Err(PricingError::new(
                        "invalid_offer",
                        Some(&component.key),
                        "component amount must not be negative",
                    ));
                }
                Some(input)
            }
            AmountRule::Lookup { input, prices } => {
                if prices.is_empty() || prices.values().any(|price| *price < 0) {
                    return Err(PricingError::new(
                        "invalid_offer",
                        Some(&component.key),
                        "lookup prices must be non-empty and non-negative",
                    ));
                }
                Some(input)
            }
            AmountRule::Graduated { input, tiers } | AmountRule::Volume { input, tiers } => {
                validate_tiers(tiers).map_err(|message| {
                    PricingError::new("invalid_offer", Some(&component.key), message)
                })?;
                has_tiered_amount = true;
                Some(input)
            }
            AmountRule::Package {
                input,
                units_per_package,
                package_amount_minor,
                ..
            } => {
                if *units_per_package == 0 || *package_amount_minor < 0 {
                    return Err(PricingError::new(
                        "invalid_offer",
                        Some(&component.key),
                        "package size must be positive and its amount must not be negative",
                    ));
                }
                Some(input)
            }
        };
        if amount_key.is_some_and(|key| !keys.contains(key.as_str())) {
            return Err(PricingError::new(
                "invalid_offer",
                Some(&component.key),
                "amount rule references an undefined input",
            ));
        }
        if let Some(key) = amount_key {
            let kind = offer
                .variables
                .iter()
                .find(|variable| variable.key == *key)
                .map(|variable| variable.kind);
            let kind_matches = match &component.amount {
                AmountRule::Lookup { .. } => {
                    matches!(kind, Some(VariableKind::Select | VariableKind::Text))
                }
                AmountRule::Fixed { .. } => true,
                _ => matches!(kind, Some(VariableKind::Number | VariableKind::Integer)),
            };
            if !kind_matches {
                return Err(PricingError::new(
                    "invalid_offer",
                    Some(&component.key),
                    "amount rule references an incompatible input type",
                ));
            }
        }
        if let QuantityRule::FromInput {
            input,
            minimum,
            maximum,
        } = &component.quantity
        {
            if !keys.contains(input.as_str())
                || *minimum == 0
                || maximum.is_some_and(|maximum| maximum < *minimum)
                || !offer.variables.iter().any(|variable| {
                    variable.key == *input
                        && matches!(variable.kind, VariableKind::Number | VariableKind::Integer)
                })
            {
                return Err(PricingError::new(
                    "invalid_offer",
                    Some(&component.key),
                    "quantity rule is invalid",
                ));
            }
        }
    }
    if has_tiered_amount && !matches!(offer.billing_scheme, BillingScheme::Tiered) {
        return Err(PricingError::new(
            "invalid_offer",
            Some("billing_scheme"),
            "graduated and volume rules require the tiered billing scheme",
        ));
    }
    Ok(())
}

pub fn evaluate_offer(
    offer: &Offer,
    request: &PricingPreviewRequest,
) -> Result<PricingPreview, PricingError> {
    validate_offer(offer)?;
    if request.offer_id != offer.id {
        return Err(PricingError::new(
            "offer_mismatch",
            Some("offer_id"),
            "request does not match the loaded offer",
        ));
    }
    if request.quantity == 0 || request.quantity > MAX_ORDER_QUANTITY {
        return Err(PricingError::new(
            "invalid_quantity",
            Some("quantity"),
            format!("quantity must be between 1 and {MAX_ORDER_QUANTITY}"),
        ));
    }
    let inputs = validate_inputs(&offer.variables, &request.inputs)?;
    let mut ordered: Vec<_> = offer.components.iter().collect();
    ordered.sort_by(|left, right| {
        left.sort_order
            .cmp(&right.sort_order)
            .then_with(|| left.key.cmp(&right.key))
    });
    let mut subtotal = 0_i64;
    let mut components = Vec::with_capacity(ordered.len());
    for component in ordered {
        if !evaluate_condition(&component.condition, &inputs)? {
            components.push(ResolvedComponent {
                component_id: component.id.clone(),
                key: component.key.clone(),
                label: component.label.clone(),
                included: false,
                required: component.required,
                unit_amount_minor: 0,
                quantity: 0,
                total_amount_minor: 0,
                reason: "condition_not_met".to_string(),
            });
            continue;
        }
        let unit_amount = resolve_amount(&component.amount, &inputs)?;
        let component_quantity = resolve_quantity(&component.quantity, &inputs)?;
        let quantity = component_quantity
            .checked_mul(request.quantity)
            .ok_or_else(|| {
                PricingError::new(
                    "amount_overflow",
                    Some(&component.key),
                    "quantity is too large",
                )
            })?;
        let total = unit_amount
            .checked_mul(i64::try_from(quantity).map_err(|_| {
                PricingError::new(
                    "amount_overflow",
                    Some(&component.key),
                    "quantity is too large",
                )
            })?)
            .ok_or_else(|| {
                PricingError::new(
                    "amount_overflow",
                    Some(&component.key),
                    "calculated amount is too large",
                )
            })?;
        subtotal = subtotal.checked_add(total).ok_or_else(|| {
            PricingError::new(
                "amount_overflow",
                Some(&component.key),
                "offer total is too large",
            )
        })?;
        components.push(ResolvedComponent {
            component_id: component.id.clone(),
            key: component.key.clone(),
            label: component.label.clone(),
            included: true,
            required: component.required,
            unit_amount_minor: unit_amount,
            quantity,
            total_amount_minor: total,
            reason: "included".to_string(),
        });
    }
    if subtotal <= 0 {
        return Err(PricingError::new(
            "invalid_total",
            None,
            "offer total must be greater than zero",
        ));
    }
    if offer
        .checkout
        .minimum_total_minor
        .is_some_and(|minimum| subtotal < minimum)
    {
        return Err(PricingError::new(
            "total_below_minimum",
            Some("checkout.minimum_total_minor"),
            "evaluated item total is below the offer minimum",
        ));
    }
    if offer
        .checkout
        .maximum_total_minor
        .is_some_and(|maximum| subtotal > maximum)
    {
        return Err(PricingError::new(
            "total_above_maximum",
            Some("checkout.maximum_total_minor"),
            "evaluated item total is above the offer maximum",
        ));
    }
    Ok(PricingPreview {
        schema_version: COMMERCE_SCHEMA_VERSION,
        offer_id: offer.id.clone(),
        offer_version: offer.version,
        quantity: request.quantity,
        inputs: inputs.normalized(),
        components,
        amounts: MoneyBreakdown {
            currency: normalize_currency(&offer.currency).expect("validated currency"),
            subtotal_minor: subtotal,
            discount_minor: 0,
            tax_minor: 0,
            shipping_minor: 0,
            platform_fee_minor: 0,
            total_minor: subtotal,
        },
    })
}
