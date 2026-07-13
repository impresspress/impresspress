use std::collections::HashMap;

use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, InputStream, OutputStream};

use super::{PRICING_TABLE, PRODUCTS_TABLE};
use crate::{
    http::{err_bad_request, err_internal_no_cause, err_not_found, ok_json},
    util::RecordExt,
};

/// Pricing template table — reusable pricing rule definitions (formulas,
/// tiers, etc.) referenced by products at calc time.
pub(crate) const TABLE: &str = "impresspress__products__pricing_templates";

/// What to do when a product references a `pricing_template_id` whose row is
/// absent from the pricing-templates table.
///
/// The price-preview endpoint (`handle_calculate`) treats this as a data
/// integrity error (`MissingTemplate::Error`) so the broken reference is
/// surfaced; the purchase path (`handle_create`) intentionally falls back to
/// the product's `base_price` (`MissingTemplate::FallBackToBase`) so a stale
/// template reference can't block a sale — both behaviors are pinned by tests.
#[derive(Clone, Copy)]
pub enum MissingTemplate {
    /// Return `Err` describing the missing template.
    Error,
    /// Use the product's `base_price` instead.
    FallBackToBase,
}

/// A resolved unit price plus the pricing formula that produced it (if any).
pub struct ResolvedPrice {
    /// The unit price, guaranteed to have passed [`validate_price`].
    pub unit_price: f64,
    /// The template's `price_formula` when a pricing template was applied;
    /// `None` when the product's `base_price` was used.
    pub formula: Option<String>,
}

/// Read a product's `base_price`, defaulting to `0.0` when absent/non-numeric.
/// `validate_price` downstream rejects the `0.0` placeholder.
fn base_price(product: &Record) -> f64 {
    product
        .data
        .get("base_price")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

/// Resolve the unit price for `product`, applying its pricing template (if it
/// references one) against `variables`, else its `base_price`.
///
/// Single source of truth for unit-price resolution shared by the
/// price-preview endpoint and the purchase path. The returned price is
/// guaranteed to have passed [`validate_price`]. `on_missing_template` picks
/// the behavior when a referenced template row is absent (see
/// [`MissingTemplate`]).
pub async fn resolve_unit_price(
    ctx: &dyn Context,
    product: &Record,
    variables: &HashMap<String, f64>,
    on_missing_template: MissingTemplate,
) -> Result<ResolvedPrice, String> {
    let template_id = product.str_field("pricing_template_id");

    let (price, formula) = if template_id.is_empty() {
        (base_price(product), None)
    } else {
        match db::get(ctx, PRICING_TABLE, template_id).await {
            Ok(template) => {
                let formula = template.str_field("price_formula").to_string();
                if formula.is_empty() {
                    return Err("Empty pricing formula".to_string());
                }
                let price = evaluate_formula(&formula, variables)
                    .map_err(|e| format!("Formula evaluation error: {e}"))?;
                (price, Some(formula))
            }
            Err(_) => match on_missing_template {
                MissingTemplate::Error => return Err("Pricing template not found".to_string()),
                MissingTemplate::FallBackToBase => (base_price(product), None),
            },
        }
    };

    validate_price(price)?;
    Ok(ResolvedPrice {
        unit_price: price,
        formula,
    })
}

pub async fn handle_calculate(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct CalcReq {
        product_id: String,
        #[serde(default)]
        variables: HashMap<String, f64>,
        #[serde(default = "default_quantity")]
        quantity: i64,
    }
    fn default_quantity() -> i64 {
        1
    }

    let raw = input.collect_to_bytes().await;
    let body: CalcReq = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Get product
    let Ok(product) = db::get(ctx, PRODUCTS_TABLE, &body.product_id).await else {
        return err_not_found("Product not found");
    };

    let resolved =
        match resolve_unit_price(ctx, &product, &body.variables, MissingTemplate::Error).await {
            Ok(r) => r,
            // A missing template row / empty formula is a server-side data
            // integrity problem; a bad formula or sub-minimum price is a
            // client-correctable bad request.
            Err(e) if e == "Pricing template not found" || e == "Empty pricing formula" => {
                return err_internal_no_cause(&e)
            }
            Err(e) => return err_bad_request(&e),
        };

    let total = resolved.unit_price * body.quantity as f64;
    let currency = product
        .data
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USD");

    match resolved.formula {
        Some(formula) => ok_json(&serde_json::json!({
            "unit_price": resolved.unit_price,
            "quantity": body.quantity,
            "total": total,
            "currency": currency,
            "formula": formula,
            "variables_used": body.variables
        })),
        None => ok_json(&serde_json::json!({
            "unit_price": resolved.unit_price,
            "quantity": body.quantity,
            "total": total,
            "currency": currency
        })),
    }
}

/// Minimum acceptable price for a product (in display currency units).
/// Anything below this is treated as an attempt at price manipulation or a
/// formula bug, not a legitimate sale.
pub const MIN_PRICE: f64 = 0.01;

/// Validate an evaluated unit/final price.
///
/// Rejects NaN, non-finite values, and any price <= 0.0. Enforces a minimum
/// of `MIN_PRICE` (1 cent). Returns a human-readable error suitable for
/// `err_bad_request`.
pub fn validate_price(price: f64) -> Result<(), String> {
    if !price.is_finite() {
        return Err("Invalid price: not a finite number".to_string());
    }
    if price <= 0.0 {
        return Err("Invalid price: must be greater than zero".to_string());
    }
    if price < MIN_PRICE {
        return Err(format!(
            "Invalid price: must be at least {MIN_PRICE} (got {price})"
        ));
    }
    Ok(())
}

/// Evaluate a simple pricing formula.
/// Supports: numbers, +, -, *, /, parentheses, and variable references.
/// Variables are referenced by name: `base_price * quantity + shipping`
pub fn evaluate_formula(formula: &str, variables: &HashMap<String, f64>) -> Result<f64, String> {
    let tokens = tokenize(formula)?;
    let mut pos = 0;
    let result = parse_expression(&tokens, &mut pos, variables)?;
    // Reject input the parser stopped short on (e.g. "2 3", "(1+2))",
    // "base_price 5") instead of silently returning a partial result.
    if pos != tokens.len() {
        return Err("Unexpected trailing tokens in formula".to_string());
    }
    Ok(result)
}

#[derive(Debug, Clone)]
enum Token {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' => {
                chars.next();
            }
            '+' => {
                chars.next();
                tokens.push(Token::Plus);
            }
            '-' => {
                chars.next();
                tokens.push(Token::Minus);
            }
            '*' => {
                chars.next();
                tokens.push(Token::Star);
            }
            '/' => {
                chars.next();
                tokens.push(Token::Slash);
            }
            '(' => {
                chars.next();
                tokens.push(Token::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            c if c.is_ascii_digit() || c == '.' => {
                let mut num_str = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() || c == '.' {
                        num_str.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let num = num_str
                    .parse::<f64>()
                    .map_err(|e| format!("Invalid number: {e}"))?;
                tokens.push(Token::Number(num));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        ident.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Ident(ident));
            }
            c => return Err(format!("Unexpected character: {c}")),
        }
    }
    Ok(tokens)
}

fn parse_expression(
    tokens: &[Token],
    pos: &mut usize,
    vars: &HashMap<String, f64>,
) -> Result<f64, String> {
    let mut left = parse_term(tokens, pos, vars)?;
    while *pos < tokens.len() {
        match &tokens[*pos] {
            Token::Plus => {
                *pos += 1;
                left += parse_term(tokens, pos, vars)?;
            }
            Token::Minus => {
                *pos += 1;
                left -= parse_term(tokens, pos, vars)?;
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_term(
    tokens: &[Token],
    pos: &mut usize,
    vars: &HashMap<String, f64>,
) -> Result<f64, String> {
    let mut left = parse_factor(tokens, pos, vars)?;
    while *pos < tokens.len() {
        match &tokens[*pos] {
            Token::Star => {
                *pos += 1;
                left *= parse_factor(tokens, pos, vars)?;
            }
            Token::Slash => {
                *pos += 1;
                let right = parse_factor(tokens, pos, vars)?;
                if right == 0.0 {
                    return Err("Division by zero".to_string());
                }
                left /= right;
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_factor(
    tokens: &[Token],
    pos: &mut usize,
    vars: &HashMap<String, f64>,
) -> Result<f64, String> {
    if *pos >= tokens.len() {
        return Err("Unexpected end of expression".to_string());
    }
    match &tokens[*pos] {
        Token::Number(n) => {
            let v = *n;
            *pos += 1;
            Ok(v)
        }
        Token::Ident(name) => {
            *pos += 1;
            vars.get(name)
                .copied()
                .ok_or_else(|| format!("Unknown variable: {name}"))
        }
        Token::LParen => {
            *pos += 1;
            let val = parse_expression(tokens, pos, vars)?;
            if *pos < tokens.len() {
                if let Token::RParen = &tokens[*pos] {
                    *pos += 1;
                } else {
                    return Err("Expected closing parenthesis".to_string());
                }
            } else {
                return Err("Missing closing parenthesis".to_string());
            }
            Ok(val)
        }
        Token::Minus => {
            *pos += 1;
            let val = parse_factor(tokens, pos, vars)?;
            Ok(-val)
        }
        _ => Err(format!("Unexpected token at position {pos}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> HashMap<String, f64> {
        HashMap::from([
            ("base_price".to_string(), 10.0),
            ("quantity".to_string(), 3.0),
        ])
    }

    #[test]
    fn evaluate_formula_computes_valid_expression() {
        assert_eq!(
            evaluate_formula("base_price * quantity", &vars()).unwrap(),
            30.0
        );
        assert_eq!(evaluate_formula("(1 + 2) * 4", &vars()).unwrap(), 12.0);
    }

    #[test]
    fn evaluate_formula_rejects_trailing_tokens() {
        // Each parses a valid leading sub-expression but leaves tokens behind;
        // before the fix these silently returned the partial result.
        for bad in ["2 3", "(1+2))", "base_price 5", "1 + 2 foo"] {
            assert!(
                evaluate_formula(bad, &vars()).is_err(),
                "formula {bad:?} must be rejected, not silently truncated"
            );
        }
    }
}
