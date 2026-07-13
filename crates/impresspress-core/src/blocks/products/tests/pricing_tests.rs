use std::collections::HashMap;

use wafer_run::ErrorCode;

use super::harness::*;
use crate::blocks::products::pricing::{evaluate_formula, validate_price, MIN_PRICE};

// ============================================================
// Basic arithmetic
// ============================================================

#[test]
fn formula_integer() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("42", &vars).unwrap(), 42.0);
}

#[test]
fn formula_decimal() {
    let vars = HashMap::new();
    assert!((evaluate_formula("1.25", &vars).unwrap() - 1.250).abs() < 1e-10);
}

#[test]
fn formula_addition() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("2 + 3", &vars).unwrap(), 5.0);
}

#[test]
fn formula_subtraction() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("10 - 4", &vars).unwrap(), 6.0);
}

#[test]
fn formula_multiplication() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("6 * 7", &vars).unwrap(), 42.0);
}

#[test]
fn formula_division() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("20 / 4", &vars).unwrap(), 5.0);
}

#[test]
fn formula_division_by_zero() {
    let vars = HashMap::new();
    let err = evaluate_formula("10 / 0", &vars).unwrap_err();
    assert!(err.contains("Division by zero"));
}

// ============================================================
// Operator precedence
// ============================================================

#[test]
fn formula_precedence_mul_before_add() {
    let vars = HashMap::new();
    // 2 + 3 * 4 = 2 + 12 = 14
    assert_eq!(evaluate_formula("2 + 3 * 4", &vars).unwrap(), 14.0);
}

#[test]
fn formula_precedence_div_before_sub() {
    let vars = HashMap::new();
    // 10 - 6 / 2 = 10 - 3 = 7
    assert_eq!(evaluate_formula("10 - 6 / 2", &vars).unwrap(), 7.0);
}

#[test]
fn formula_chained_operations() {
    let vars = HashMap::new();
    // 1 + 2 + 3 + 4 = 10
    assert_eq!(evaluate_formula("1 + 2 + 3 + 4", &vars).unwrap(), 10.0);
}

#[test]
fn formula_mixed_precedence() {
    let vars = HashMap::new();
    // 2 * 3 + 4 * 5 = 6 + 20 = 26
    assert_eq!(evaluate_formula("2 * 3 + 4 * 5", &vars).unwrap(), 26.0);
}

// ============================================================
// Parentheses
// ============================================================

#[test]
fn formula_parentheses_override_precedence() {
    let vars = HashMap::new();
    // (2 + 3) * 4 = 5 * 4 = 20
    assert_eq!(evaluate_formula("(2 + 3) * 4", &vars).unwrap(), 20.0);
}

#[test]
fn formula_nested_parentheses() {
    let vars = HashMap::new();
    // ((2 + 3) * (4 - 1)) = 5 * 3 = 15
    assert_eq!(
        evaluate_formula("((2 + 3) * (4 - 1))", &vars).unwrap(),
        15.0
    );
}

#[test]
fn formula_deeply_nested() {
    let vars = HashMap::new();
    // (((10))) = 10
    assert_eq!(evaluate_formula("(((10)))", &vars).unwrap(), 10.0);
}

#[test]
fn formula_missing_closing_paren() {
    let vars = HashMap::new();
    let err = evaluate_formula("(2 + 3", &vars).unwrap_err();
    assert!(
        err.contains("parenthesis"),
        "Expected parenthesis error, got: {err}"
    );
}

// ============================================================
// Variables
// ============================================================

#[test]
fn formula_single_variable() {
    let mut vars = HashMap::new();
    vars.insert("price".to_string(), 19.99);
    assert!((evaluate_formula("price", &vars).unwrap() - 19.99).abs() < 1e-10);
}

#[test]
fn formula_variable_arithmetic() {
    let mut vars = HashMap::new();
    vars.insert("base_price".to_string(), 10.0);
    vars.insert("quantity".to_string(), 5.0);
    assert_eq!(
        evaluate_formula("base_price * quantity", &vars).unwrap(),
        50.0
    );
}

#[test]
fn formula_complex_with_variables() {
    let mut vars = HashMap::new();
    vars.insert("base_price".to_string(), 100.0);
    vars.insert("discount".to_string(), 20.0);
    vars.insert("quantity".to_string(), 3.0);
    // base_price * quantity * (1 - discount / 100) = 100 * 3 * 0.8 = 240
    let result = evaluate_formula("base_price * quantity * (1 - discount / 100)", &vars).unwrap();
    assert!((result - 240.0).abs() < 1e-10);
}

#[test]
fn formula_unknown_variable() {
    let vars = HashMap::new();
    let err = evaluate_formula("unknown_var", &vars).unwrap_err();
    assert!(
        err.contains("Unknown variable"),
        "Expected unknown variable error, got: {err}"
    );
}

#[test]
fn formula_variable_with_underscores() {
    let mut vars = HashMap::new();
    vars.insert("my_long_variable_name".to_string(), 42.0);
    assert_eq!(
        evaluate_formula("my_long_variable_name", &vars).unwrap(),
        42.0
    );
}

#[test]
fn formula_variable_plus_constant() {
    let mut vars = HashMap::new();
    vars.insert("base".to_string(), 50.0);
    // base + 10.5
    assert_eq!(evaluate_formula("base + 10.5", &vars).unwrap(), 60.5);
}

// ============================================================
// Negative numbers / unary minus
// ============================================================

#[test]
fn formula_unary_minus() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("-5", &vars).unwrap(), -5.0);
}

#[test]
fn formula_unary_minus_in_expression() {
    let vars = HashMap::new();
    // 10 + -3 = 7
    assert_eq!(evaluate_formula("10 + -3", &vars).unwrap(), 7.0);
}

#[test]
fn formula_unary_minus_with_parens() {
    let vars = HashMap::new();
    // -(2 + 3) = -5
    assert_eq!(evaluate_formula("-(2 + 3)", &vars).unwrap(), -5.0);
}

// ============================================================
// Edge cases
// ============================================================

#[test]
fn formula_whitespace_handling() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("  2  +  3  ", &vars).unwrap(), 5.0);
}

#[test]
fn formula_tabs_and_newlines() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("2\t+\n3", &vars).unwrap(), 5.0);
}

#[test]
fn formula_empty_string() {
    let vars = HashMap::new();
    let err = evaluate_formula("", &vars).unwrap_err();
    assert!(
        err.contains("Unexpected end"),
        "Expected end-of-expression error, got: {err}"
    );
}

#[test]
fn formula_invalid_character() {
    let vars = HashMap::new();
    let err = evaluate_formula("2 @ 3", &vars).unwrap_err();
    assert!(
        err.contains("Unexpected character"),
        "Expected unexpected char error, got: {err}"
    );
}

#[test]
fn formula_just_zero() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("0", &vars).unwrap(), 0.0);
}

#[test]
fn formula_large_number() {
    let vars = HashMap::new();
    assert_eq!(evaluate_formula("1000000 * 1000000", &vars).unwrap(), 1e12);
}

#[test]
fn formula_decimal_precision() {
    let vars = HashMap::new();
    // Typical pricing: 19.99 * 3
    let result = evaluate_formula("19.99 * 3", &vars).unwrap();
    assert!((result - 59.97).abs() < 1e-10);
}

// ============================================================
// Real-world pricing formulas
// ============================================================

#[test]
fn formula_tiered_pricing() {
    let mut vars = HashMap::new();
    vars.insert("units".to_string(), 150.0);
    vars.insert("rate".to_string(), 0.05);
    vars.insert("base_fee".to_string(), 10.0);
    // base_fee + units * rate = 10 + 150 * 0.05 = 10 + 7.5 = 17.5
    assert_eq!(
        evaluate_formula("base_fee + units * rate", &vars).unwrap(),
        17.5
    );
}

#[test]
fn formula_percentage_discount() {
    let mut vars = HashMap::new();
    vars.insert("price".to_string(), 200.0);
    vars.insert("discount_pct".to_string(), 15.0);
    // price * (100 - discount_pct) / 100 = 200 * 85 / 100 = 170
    assert_eq!(
        evaluate_formula("price * (100 - discount_pct) / 100", &vars).unwrap(),
        170.0
    );
}

// ============================================================
// Condition evaluation (via calculate endpoint)
// ============================================================

#[tokio::test]
async fn calculate_price_direct_base_price() {
    use crate::blocks::products::pricing;

    let ctx = ctx().await;

    // Create a product with base_price, no pricing template
    let mut product_data = HashMap::new();
    product_data.insert("name".to_string(), serde_json::json!("Widget"));
    product_data.insert("base_price".to_string(), serde_json::json!(29.99));
    product_data.insert("currency".to_string(), serde_json::json!("USD"));
    seed(
        &ctx,
        "impresspress__products__products",
        "prod_1",
        product_data,
    )
    .await;

    let (_msg, input) = create_msg(
        "/b/products/calculate-price",
        "user_1",
        serde_json::json!({
            "product_id": "prod_1",
            "quantity": 3
        }),
    );

    let out = pricing::handle_calculate(&ctx, input).await;
    let body = output_to_json(out).await;
    assert!((body["unit_price"].as_f64().unwrap() - 29.99).abs() < 0.01);
    assert_eq!(body["quantity"].as_i64().unwrap(), 3);
    assert!((body["total"].as_f64().unwrap() - 89.97).abs() < 0.01);
    assert_eq!(body["currency"].as_str().unwrap(), "USD");
}

#[tokio::test]
async fn calculate_price_with_formula() {
    use crate::blocks::products::pricing;

    let ctx = ctx().await;

    // Create a pricing template
    let mut template_data = HashMap::new();
    template_data.insert("name".to_string(), serde_json::json!("per-unit"));
    template_data.insert(
        "price_formula".to_string(),
        serde_json::json!("base * rate"),
    );
    seed(
        &ctx,
        "impresspress__products__pricing_templates",
        "tmpl_1",
        template_data,
    )
    .await;

    // Create a product referencing the template
    let mut product_data = HashMap::new();
    product_data.insert("name".to_string(), serde_json::json!("Service"));
    product_data.insert(
        "pricing_template_id".to_string(),
        serde_json::json!("tmpl_1"),
    );
    product_data.insert("currency".to_string(), serde_json::json!("EUR"));
    seed(
        &ctx,
        "impresspress__products__products",
        "prod_2",
        product_data,
    )
    .await;

    let (_msg, input) = create_msg(
        "/b/products/calculate-price",
        "user_1",
        serde_json::json!({
            "product_id": "prod_2",
            "variables": { "base": 100.0, "rate": 0.15 },
            "quantity": 2
        }),
    );

    let out = pricing::handle_calculate(&ctx, input).await;
    let body = output_to_json(out).await;
    assert!((body["unit_price"].as_f64().unwrap() - 15.0).abs() < 0.01);
    assert!((body["total"].as_f64().unwrap() - 30.0).abs() < 0.01);
    assert_eq!(body["currency"].as_str().unwrap(), "EUR");
}

#[tokio::test]
async fn calculate_price_product_not_found() {
    use crate::blocks::products::pricing;

    let ctx = ctx().await;
    let (_msg, input) = create_msg(
        "/b/products/calculate-price",
        "user_1",
        serde_json::json!({ "product_id": "nonexistent" }),
    );

    let out = pricing::handle_calculate(&ctx, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

// ============================================================
// SEC-063: validate_price guard (regression test)
// ============================================================

#[test]
fn validate_price_rejects_negative() {
    let err = validate_price(-1.0).unwrap_err();
    assert!(err.contains("greater than zero"), "got: {err}");
}

#[test]
fn validate_price_rejects_zero() {
    let err = validate_price(0.0).unwrap_err();
    assert!(err.contains("greater than zero"), "got: {err}");
}

#[test]
fn validate_price_rejects_negative_zero() {
    let err = validate_price(-0.0).unwrap_err();
    assert!(err.contains("greater than zero"), "got: {err}");
}

#[test]
fn validate_price_rejects_below_minimum() {
    let err = validate_price(0.005).unwrap_err();
    assert!(err.contains("at least"), "got: {err}");
}

#[test]
fn validate_price_accepts_minimum() {
    assert!(validate_price(MIN_PRICE).is_ok());
}

#[test]
fn validate_price_accepts_normal() {
    assert!(validate_price(9.99).is_ok());
    assert!(validate_price(1_000_000.0).is_ok());
}

#[test]
fn validate_price_rejects_nan() {
    let err = validate_price(f64::NAN).unwrap_err();
    assert!(err.contains("finite"), "got: {err}");
}

#[test]
fn validate_price_rejects_infinity() {
    let err = validate_price(f64::INFINITY).unwrap_err();
    assert!(err.contains("finite"), "got: {err}");
}
