//! A small, total expression language for panel bindings.
//!
//! A panel is data, and a value in a panel is either a literal or a binding.
//! The binding language is deliberately not a scripting language: no loops, no
//! recursion, no user-defined names, no function calls, no side effects. It is
//! a tree of operators over JSON values, which means it can be parsed, counted,
//! and type-checked **before** anything renders.
//!
//! That is the whole point. A bad binding fails at approval time with a message
//! the agent reads in its receipt, instead of rendering `undefined` in a panel
//! the user is looking at and nobody can explain.
//!
//! The design follows CEL's constraint — deterministic, bounded, side-effect
//! free — without CEL's surface. There is no macro expansion, no comprehension,
//! no custom function registry, so the evaluator (Phase 2) is a `match` over
//! eight shapes and the cost of any expression is its node count.
//!
//! Grammar:
//!
//! | Group | Operators |
//! |---|---|
//! | Path | `@path` |
//! | Compare | `@eq @ne @gt @gte @lt @lte` |
//! | Boolean | `@and @or @not` |
//! | Presence | `@exists @missing` |
//! | Arithmetic | `@add @sub @mul @div` |
//! | Format | `@format` with `@of` |

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

/// Deepest an expression may nest. A binding is a label, not a program.
pub const MAX_EXPR_DEPTH: usize = 8;

/// Most nodes an expression may contain, counted across the whole tree.
pub const MAX_EXPR_NODES: usize = 64;

/// Longest a `@path` may be, and the most segments it may have.
pub const MAX_PATH_LEN: usize = 200;
pub const MAX_PATH_SEGMENTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl CompareOp {
    pub const ALL: [&'static str; 6] = ["@eq", "@ne", "@gt", "@gte", "@lt", "@lte"];

    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "@eq" => CompareOp::Eq,
            "@ne" => CompareOp::Ne,
            "@gt" => CompareOp::Gt,
            "@gte" => CompareOp::Gte,
            "@lt" => CompareOp::Lt,
            "@lte" => CompareOp::Lte,
            _ => return None,
        })
    }

    fn token(self) -> &'static str {
        match self {
            CompareOp::Eq => "@eq",
            CompareOp::Ne => "@ne",
            CompareOp::Gt => "@gt",
            CompareOp::Gte => "@gte",
            CompareOp::Lt => "@lt",
            CompareOp::Lte => "@lte",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicOp {
    And,
    Or,
}

impl LogicOp {
    pub const ALL: [&'static str; 2] = ["@and", "@or"];

    fn parse(token: &str) -> Option<Self> {
        match token {
            "@and" => Some(LogicOp::And),
            "@or" => Some(LogicOp::Or),
            _ => None,
        }
    }

    fn token(self) -> &'static str {
        match self {
            LogicOp::And => "@and",
            LogicOp::Or => "@or",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl ArithOp {
    pub const ALL: [&'static str; 4] = ["@add", "@sub", "@mul", "@div"];

    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "@add" => ArithOp::Add,
            "@sub" => ArithOp::Sub,
            "@mul" => ArithOp::Mul,
            "@div" => ArithOp::Div,
            _ => return None,
        })
    }

    fn token(self) -> &'static str {
        match self {
            ArithOp::Add => "@add",
            ArithOp::Sub => "@sub",
            ArithOp::Mul => "@mul",
            ArithOp::Div => "@div",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    Number,
    Currency,
    Percent,
    Duration,
    Date,
    Truncate,
}

impl FormatKind {
    pub const ALL: [&'static str; 6] = [
        "number",
        "currency",
        "percent",
        "duration",
        "date",
        "truncate",
    ];

    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "number" => FormatKind::Number,
            "currency" => FormatKind::Currency,
            "percent" => FormatKind::Percent,
            "duration" => FormatKind::Duration,
            "date" => FormatKind::Date,
            "truncate" => FormatKind::Truncate,
            _ => return None,
        })
    }

    fn token(self) -> &'static str {
        match self {
            FormatKind::Number => "number",
            FormatKind::Currency => "currency",
            FormatKind::Percent => "percent",
            FormatKind::Duration => "duration",
            FormatKind::Date => "date",
            FormatKind::Truncate => "truncate",
        }
    }
}

/// What an expression is known to produce.
///
/// `Any` is the honest answer for a `@path`: the shape of the data is not known
/// until a source answers. The checker therefore rejects only what is certainly
/// wrong — `@gt` over two string literals, `@and` over a number — and lets
/// `Any` through everywhere. A checker that guessed would reject valid panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Any,
    Number,
    String,
    Bool,
    Null,
}

impl Type {
    fn name(self) -> &'static str {
        match self {
            Type::Any => "any",
            Type::Number => "number",
            Type::String => "string",
            Type::Bool => "boolean",
            Type::Null => "null",
        }
    }

    fn is_number(self) -> bool {
        matches!(self, Type::Any | Type::Number)
    }

    fn is_bool(self) -> bool {
        matches!(self, Type::Any | Type::Bool)
    }

    fn is_ordered(self) -> bool {
        matches!(self, Type::Any | Type::Number | Type::String)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Value),
    Path(String),
    Compare {
        op: CompareOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Logic {
        op: LogicOp,
        args: Vec<Expr>,
    },
    Not(Box<Expr>),
    Presence {
        path: String,
        negated: bool,
    },
    Arith {
        op: ArithOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Format {
        kind: FormatKind,
        of: Box<Expr>,
    },
    /// `{ "@if": [condition, then, else] }`.
    ///
    /// The one operator that chooses rather than computes. Without it a field
    /// can only be a literal or a path, so "this node is `danger` when the
    /// service is down" is not expressible — the source would have to emit a
    /// tone string, which is a contract no source should have to know about.
    Conditional {
        test: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
}

impl Expr {
    /// Parse and type-check in one step. A caller that only wants the shape can
    /// use `parse` and inspect the result.
    pub fn checked(value: &Value) -> Result<Expr, String> {
        let expr = Expr::parse(value)?;
        expr.check()?;
        Ok(expr)
    }

    pub fn parse(value: &Value) -> Result<Expr, String> {
        let mut budget = MAX_EXPR_NODES;
        Self::parse_at(value, 0, &mut budget)
    }

    fn parse_at(value: &Value, depth: usize, budget: &mut usize) -> Result<Expr, String> {
        if depth > MAX_EXPR_DEPTH {
            return Err(format!(
                "expression nests deeper than {MAX_EXPR_DEPTH} levels"
            ));
        }
        if *budget == 0 {
            return Err(format!(
                "expression has more than {MAX_EXPR_NODES} nodes"
            ));
        }
        *budget -= 1;

        let Value::Object(map) = value else {
            return Ok(Expr::Literal(value.clone()));
        };

        // `@format` is the one operator that takes a second key, so it is
        // matched before the single-key rule.
        if let Some(kind) = map.get("@format") {
            if map.len() != 2 || !map.contains_key("@of") {
                return Err("`@format` takes exactly `@format` and `@of`".into());
            }
            let kind = kind
                .as_str()
                .and_then(FormatKind::parse)
                .ok_or_else(|| {
                    format!(
                        "`@format` must be one of {}",
                        FormatKind::ALL.join(", ")
                    )
                })?;
            let of = Self::parse_at(&map["@of"], depth + 1, budget)?;
            return Ok(Expr::Format {
                kind,
                of: Box::new(of),
            });
        }

        if map.len() != 1 {
            return Err(
                "an expression object has exactly one operator key, for example `{ \"@path\": \"/a/b\" }`"
                    .into(),
            );
        }
        let (token, argument) = map.iter().next().expect("checked len 1");

        if token == "@path" {
            let path = argument
                .as_str()
                .ok_or_else(|| "`@path` takes a string".to_string())?;
            check_path(path)?;
            return Ok(Expr::Path(path.to_string()));
        }

        if token == "@if" {
            let items = argument.as_array().ok_or_else(|| {
                "`@if` takes a three-item array: [condition, then, else]".to_string()
            })?;
            if items.len() != 3 {
                return Err(format!(
                    "`@if` takes exactly three items — condition, then, else — not {}",
                    items.len()
                ));
            }
            return Ok(Expr::Conditional {
                test: Box::new(Self::parse_at(&items[0], depth + 1, budget)?),
                then: Box::new(Self::parse_at(&items[1], depth + 1, budget)?),
                otherwise: Box::new(Self::parse_at(&items[2], depth + 1, budget)?),
            });
        }

        if let Some(op) = CompareOp::parse(token) {
            let (left, right) = pair(argument, token)?;
            return Ok(Expr::Compare {
                op,
                left: Box::new(Self::parse_at(left, depth + 1, budget)?),
                right: Box::new(Self::parse_at(right, depth + 1, budget)?),
            });
        }

        if let Some(op) = ArithOp::parse(token) {
            let (left, right) = pair(argument, token)?;
            return Ok(Expr::Arith {
                op,
                left: Box::new(Self::parse_at(left, depth + 1, budget)?),
                right: Box::new(Self::parse_at(right, depth + 1, budget)?),
            });
        }

        if let Some(op) = LogicOp::parse(token) {
            let items = argument
                .as_array()
                .ok_or_else(|| format!("`{token}` takes an array of expressions"))?;
            if items.len() < 2 {
                return Err(format!("`{token}` needs at least two operands"));
            }
            let mut args = Vec::with_capacity(items.len());
            for item in items {
                args.push(Self::parse_at(item, depth + 1, budget)?);
            }
            return Ok(Expr::Logic { op, args });
        }

        match token.as_str() {
            "@not" => Ok(Expr::Not(Box::new(Self::parse_at(
                argument,
                depth + 1,
                budget,
            )?))),
            "@exists" | "@missing" => {
                let path = argument
                    .as_str()
                    .ok_or_else(|| format!("`{token}` takes a path string"))?;
                check_path(path)?;
                Ok(Expr::Presence {
                    path: path.to_string(),
                    negated: token == "@missing",
                })
            }
            other => Err(format!(
                "unknown operator `{other}`; the operators are {}",
                operators().join(", ")
            )),
        }
    }

    /// Infer the type, rejecting only what is certainly wrong.
    pub fn check(&self) -> Result<Type, String> {
        match self {
            Expr::Literal(value) => Ok(match value {
                Value::Number(_) => Type::Number,
                Value::String(_) => Type::String,
                Value::Bool(_) => Type::Bool,
                Value::Null => Type::Null,
                _ => Type::Any,
            }),
            Expr::Path(_) => Ok(Type::Any),
            Expr::Presence { .. } => Ok(Type::Bool),
            Expr::Compare { op, left, right } => {
                let left_ty = left.check()?;
                let right_ty = right.check()?;
                let ordered = matches!(op, CompareOp::Gt | CompareOp::Gte | CompareOp::Lt | CompareOp::Lte);
                if ordered {
                    if !left_ty.is_ordered() {
                        return Err(format!(
                            "`{}` compares numbers or strings, but the left side is {}",
                            op.token(),
                            left_ty.name()
                        ));
                    }
                    if !right_ty.is_ordered() {
                        return Err(format!(
                            "`{}` compares numbers or strings, but the right side is {}",
                            op.token(),
                            right_ty.name()
                        ));
                    }
                }
                Ok(Type::Bool)
            }
            Expr::Logic { op, args } => {
                for (index, arg) in args.iter().enumerate() {
                    let ty = arg.check()?;
                    if !ty.is_bool() {
                        return Err(format!(
                            "`{}` takes booleans, but operand {} is {}",
                            op.token(),
                            index + 1,
                            ty.name()
                        ));
                    }
                }
                Ok(Type::Bool)
            }
            Expr::Not(inner) => {
                let ty = inner.check()?;
                if !ty.is_bool() {
                    return Err(format!("`@not` takes a boolean, but got {}", ty.name()));
                }
                Ok(Type::Bool)
            }
            Expr::Arith { op, left, right } => {
                for (side, expr) in [("left", left), ("right", right)] {
                    let ty = expr.check()?;
                    if !ty.is_number() {
                        return Err(format!(
                            "`{}` takes numbers, but the {side} side is {}",
                            op.token(),
                            ty.name()
                        ));
                    }
                }
                Ok(Type::Number)
            }
            Expr::Format { of, .. } => {
                of.check()?;
                Ok(Type::String)
            }
            Expr::Conditional {
                test,
                then,
                otherwise,
            } => {
                let test_ty = test.check()?;
                if !test_ty.is_bool() {
                    return Err(format!(
                        "`@if` takes a condition first, but got {}",
                        test_ty.name()
                    ));
                }
                let then_ty = then.check()?;
                let else_ty = otherwise.check()?;
                // Both branches the same type is the only case worth promising.
                Ok(if then_ty == else_ty { then_ty } else { Type::Any })
            }
        }
    }

    /// The type a `when` field must have.
    pub fn check_condition(&self) -> Result<(), String> {
        let ty = self.check()?;
        if ty.is_bool() {
            Ok(())
        } else {
            Err(format!(
                "`when` must be a condition, but this one is {}",
                ty.name()
            ))
        }
    }

    /// Every path the expression reads, in first-seen order.
    ///
    /// Phase 2 uses this to decide which sources a panel actually needs, so a
    /// panel that binds nothing does not poll anything.
    pub fn paths(&self) -> Vec<String> {
        let mut found = Vec::new();
        self.collect_paths(&mut found);
        found
    }

    fn collect_paths(&self, found: &mut Vec<String>) {
        match self {
            Expr::Literal(_) => {}
            Expr::Path(path) => push_unique(found, path),
            Expr::Presence { path, .. } => push_unique(found, path),
            Expr::Compare { left, right, .. } | Expr::Arith { left, right, .. } => {
                left.collect_paths(found);
                right.collect_paths(found);
            }
            Expr::Logic { args, .. } => {
                for arg in args {
                    arg.collect_paths(found);
                }
            }
            Expr::Not(inner) => inner.collect_paths(found),
            Expr::Format { of, .. } => of.collect_paths(found),
            Expr::Conditional {
                test,
                then,
                otherwise,
            } => {
                test.collect_paths(found);
                then.collect_paths(found);
                otherwise.collect_paths(found);
            }
        }
    }

    /// Rebuild the JSON this expression was parsed from.
    pub fn to_value(&self) -> Value {
        match self {
            Expr::Literal(value) => value.clone(),
            Expr::Path(path) => single("@path", Value::String(path.clone())),
            Expr::Presence { path, negated } => single(
                if *negated { "@missing" } else { "@exists" },
                Value::String(path.clone()),
            ),
            Expr::Compare { op, left, right } => {
                single(op.token(), Value::Array(vec![left.to_value(), right.to_value()]))
            }
            Expr::Arith { op, left, right } => {
                single(op.token(), Value::Array(vec![left.to_value(), right.to_value()]))
            }
            Expr::Logic { op, args } => single(
                op.token(),
                Value::Array(args.iter().map(Expr::to_value).collect()),
            ),
            Expr::Not(inner) => single("@not", inner.to_value()),
            Expr::Format { kind, of } => {
                let mut map = Map::new();
                map.insert("@format".into(), Value::String(kind.token().into()));
                map.insert("@of".into(), of.to_value());
                Value::Object(map)
            }
            Expr::Conditional {
                test,
                then,
                otherwise,
            } => single(
                "@if",
                Value::Array(vec![test.to_value(), then.to_value(), otherwise.to_value()]),
            ),
        }
    }
}

impl Serialize for Expr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Expr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Expr::checked(&value).map_err(serde::de::Error::custom)
    }
}

/// Every operator token, for the manifest.
pub fn operators() -> Vec<&'static str> {
    let mut all = vec!["@path", "@not", "@exists", "@missing", "@format", "@if"];
    all.extend(CompareOp::ALL);
    all.extend(LogicOp::ALL);
    all.extend(ArithOp::ALL);
    all
}

fn single(token: &str, argument: Value) -> Value {
    let mut map = Map::new();
    map.insert(token.to_string(), argument);
    Value::Object(map)
}

fn pair<'a>(argument: &'a Value, token: &str) -> Result<(&'a Value, &'a Value), String> {
    let items = argument
        .as_array()
        .ok_or_else(|| format!("`{token}` takes a two-item array"))?;
    if items.len() != 2 {
        return Err(format!(
            "`{token}` takes exactly two operands, not {}",
            items.len()
        ));
    }
    Ok((&items[0], &items[1]))
}

fn push_unique(list: &mut Vec<String>, path: &str) {
    if !list.iter().any(|existing| existing == path) {
        list.push(path.to_string());
    }
}

/// A path is a slash-separated walk into the panel's data model.
///
/// It is not a JSON Pointer: there is no `~0`/`~1` escaping and no array index
/// syntax, because a panel that needs to index into an array uses a `table` or
/// `chart` block, which take a whole array. Keeping the grammar this small is
/// what makes the evaluator a lookup rather than a parser.
pub fn check_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("path is empty".into());
    }
    if path.len() > MAX_PATH_LEN {
        return Err(format!("path is longer than {MAX_PATH_LEN} characters"));
    }
    if !path.starts_with('/') {
        return Err(format!("path `{path}` must start with `/`"));
    }
    if path.chars().any(|c| c.is_control()) {
        return Err("path contains control characters".into());
    }
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    if segments.len() > MAX_PATH_SEGMENTS {
        return Err(format!(
            "path has {} segments; the limit is {MAX_PATH_SEGMENTS}",
            segments.len()
        ));
    }
    for segment in segments {
        if segment.is_empty() {
            return Err(format!("path `{path}` has an empty segment"));
        }
        if segment == "." || segment == ".." {
            return Err(format!("path `{path}` must not contain `.` or `..`"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_literal_is_an_expression() {
        let expr = Expr::checked(&json!(42)).unwrap();
        assert_eq!(expr, Expr::Literal(json!(42)));
        assert_eq!(expr.check().unwrap(), Type::Number);
    }

    #[test]
    fn a_path_is_any_typed_because_the_data_is_not_known_yet() {
        let expr = Expr::checked(&json!({ "@path": "/ci/summary/state" })).unwrap();
        assert_eq!(expr.check().unwrap(), Type::Any);
        assert_eq!(expr.paths(), vec!["/ci/summary/state"]);
    }

    #[test]
    fn a_comparison_is_a_condition() {
        let expr = Expr::checked(&json!({
            "@gt": [{ "@path": "/ci/queue/depth" }, 10]
        }))
        .unwrap();
        assert_eq!(expr.check().unwrap(), Type::Bool);
        expr.check_condition().unwrap();
    }

    #[test]
    fn a_nested_condition_round_trips() {
        let raw = json!({
            "@and": [
                { "@gt": [{ "@path": "/ci/queue/depth" }, 10] },
                { "@ne": [{ "@path": "/ci/summary/state" }, "ok"] }
            ]
        });
        let expr = Expr::checked(&raw).unwrap();
        assert_eq!(expr.to_value(), raw);
        assert_eq!(
            expr.paths(),
            vec!["/ci/queue/depth", "/ci/summary/state"]
        );
    }

    #[test]
    fn a_format_wraps_its_operand() {
        let raw = json!({ "@format": "number", "@of": { "@path": "/ci/queue/depth" } });
        let expr = Expr::checked(&raw).unwrap();
        assert_eq!(expr.check().unwrap(), Type::String);
        assert_eq!(expr.to_value(), raw);
    }

    #[test]
    fn a_format_without_an_operand_is_refused() {
        let error = Expr::checked(&json!({ "@format": "number" })).unwrap_err();
        assert!(error.contains("@of"), "{error}");
    }

    #[test]
    fn an_unknown_format_is_refused_and_lists_the_real_ones() {
        let error =
            Expr::checked(&json!({ "@format": "money", "@of": 1 })).unwrap_err();
        assert!(error.contains("currency"), "{error}");
    }

    #[test]
    fn an_unknown_operator_is_refused_and_lists_the_real_ones() {
        let error = Expr::checked(&json!({ "@sum": [1, 2] })).unwrap_err();
        assert!(error.contains("unknown operator `@sum`"), "{error}");
        assert!(error.contains("@add"), "{error}");
    }

    #[test]
    fn two_operators_in_one_object_are_refused() {
        let error = Expr::checked(&json!({ "@eq": [1, 1], "@ne": [1, 2] })).unwrap_err();
        assert!(error.contains("exactly one operator key"), "{error}");
    }

    #[test]
    fn a_comparison_needs_exactly_two_operands() {
        let error = Expr::checked(&json!({ "@eq": [1, 1, 1] })).unwrap_err();
        assert!(error.contains("exactly two operands"), "{error}");
    }

    #[test]
    fn a_boolean_needs_at_least_two_operands() {
        let error = Expr::checked(&json!({ "@and": [true] })).unwrap_err();
        assert!(error.contains("at least two"), "{error}");
    }

    #[test]
    fn ordering_two_string_literals_is_allowed_but_ordering_a_boolean_is_not() {
        Expr::checked(&json!({ "@gt": ["a", "b"] })).unwrap();
        let error = Expr::checked(&json!({ "@gt": [true, false] })).unwrap_err();
        assert!(error.contains("compares numbers or strings"), "{error}");
    }

    #[test]
    fn arithmetic_over_a_string_literal_is_refused() {
        let error = Expr::checked(&json!({ "@add": [1, "two"] })).unwrap_err();
        assert!(error.contains("takes numbers"), "{error}");
    }

    #[test]
    fn a_boolean_over_a_number_literal_is_refused() {
        let error = Expr::checked(&json!({ "@and": [true, 1] })).unwrap_err();
        assert!(error.contains("takes booleans"), "{error}");
    }

    #[test]
    fn a_when_that_is_not_a_condition_is_refused() {
        let expr = Expr::checked(&json!({ "@path": "/a/b" })).unwrap();
        // `Any` is allowed: the data may well be a boolean.
        expr.check_condition().unwrap();

        let number = Expr::checked(&json!(7)).unwrap();
        let error = number.check_condition().unwrap_err();
        assert!(error.contains("must be a condition"), "{error}");
    }

    #[test]
    fn a_path_must_start_with_a_slash() {
        let error = Expr::checked(&json!({ "@path": "ci/state" })).unwrap_err();
        assert!(error.contains("must start with `/`"), "{error}");
    }

    #[test]
    fn a_path_must_not_climb() {
        let error = Expr::checked(&json!({ "@path": "/a/../b" })).unwrap_err();
        assert!(error.contains("must not contain"), "{error}");
    }

    #[test]
    fn a_path_must_not_have_an_empty_segment() {
        let error = Expr::checked(&json!({ "@path": "/a//b" })).unwrap_err();
        assert!(error.contains("empty segment"), "{error}");
    }

    #[test]
    fn a_path_must_not_be_too_deep() {
        let deep = format!("/{}", vec!["a"; MAX_PATH_SEGMENTS + 1].join("/"));
        let error = Expr::checked(&json!({ "@path": deep })).unwrap_err();
        assert!(error.contains("segments"), "{error}");
    }

    #[test]
    fn an_expression_may_not_nest_past_the_limit() {
        let mut value = json!(1);
        for _ in 0..=MAX_EXPR_DEPTH {
            value = json!({ "@not": value });
        }
        let error = Expr::checked(&value).unwrap_err();
        assert!(error.contains("nests deeper"), "{error}");
    }

    #[test]
    fn an_expression_may_not_exceed_the_node_budget() {
        let args: Vec<Value> = (0..MAX_EXPR_NODES).map(|_| json!(true)).collect();
        let error = Expr::checked(&json!({ "@and": args })).unwrap_err();
        assert!(error.contains("nodes"), "{error}");
    }

    #[test]
    fn presence_checks_are_conditions() {
        let expr = Expr::checked(&json!({ "@missing": "/ci/queue" })).unwrap();
        assert_eq!(expr.check().unwrap(), Type::Bool);
        assert_eq!(expr.paths(), vec!["/ci/queue"]);
    }

    #[test]
    fn every_operator_round_trips_through_json() {
        let samples = [
            json!({ "@path": "/a" }),
            json!({ "@exists": "/a" }),
            json!({ "@missing": "/a" }),
            json!({ "@not": true }),
            json!({ "@eq": [1, 2] }),
            json!({ "@ne": [1, 2] }),
            json!({ "@gt": [1, 2] }),
            json!({ "@gte": [1, 2] }),
            json!({ "@lt": [1, 2] }),
            json!({ "@lte": [1, 2] }),
            json!({ "@and": [true, false] }),
            json!({ "@or": [true, false] }),
            json!({ "@add": [1, 2] }),
            json!({ "@sub": [1, 2] }),
            json!({ "@mul": [1, 2] }),
            json!({ "@div": [1, 2] }),
            json!({ "@format": "percent", "@of": { "@path": "/a" } }),
        ];
        for sample in samples {
            let expr = Expr::checked(&sample).unwrap_or_else(|e| panic!("{sample}: {e}"));
            assert_eq!(expr.to_value(), sample, "round trip changed {sample}");
        }
    }

    #[test]
    fn the_manifest_operator_list_covers_every_operator() {
        let listed = operators();
        for token in CompareOp::ALL
            .iter()
            .chain(LogicOp::ALL.iter())
            .chain(ArithOp::ALL.iter())
        {
            assert!(listed.contains(token), "{token} is missing from the manifest");
        }
        assert_eq!(listed.len(), 18);
    }

    #[test]
    fn a_deserialised_expression_is_already_checked() {
        let ok: Result<Expr, _> = serde_json::from_value(json!({ "@path": "/a/b" }));
        assert!(ok.is_ok());

        let bad: Result<Expr, _> = serde_json::from_value(json!({ "@nope": 1 }));
        assert!(bad.is_err());
    }

    /// The shared table, also evaluated by the frontend. Rust's half of the
    /// contract is that every expression in it parses and type-checks: if the
    /// frontend learns an operator this parser rejects, the panel would be
    /// refused at approval time and the renderer would never see it.
    ///
    /// The frontend's half — that each one evaluates to `expect` — is checked
    /// against the same file by the CDP harness, because there is no JS test
    /// runner in this project.
    #[test]
    fn every_shared_vector_parses_and_type_checks() {
        let raw = include_str!("../../src/lib/expr-vectors.json");
        let vectors: Vec<serde_json::Value> = serde_json::from_str(raw).unwrap();
        assert!(vectors.len() >= 40, "the table shrank to {}", vectors.len());

        for vector in &vectors {
            let name = vector["name"].as_str().unwrap_or("<unnamed>");
            let expr = &vector["expr"];
            Expr::checked(expr).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    /// Every operator the frontend implements must be one the parser accepts,
    /// and the reverse. The table is the contract; this checks it covers the
    /// whole grammar rather than a convenient subset.
    #[test]
    fn the_shared_vectors_cover_every_operator() {
        let raw = include_str!("../../src/lib/expr-vectors.json");
        let vectors: Vec<serde_json::Value> = serde_json::from_str(raw).unwrap();

        let mut seen: Vec<String> = Vec::new();
        for vector in &vectors {
            for path in Expr::checked(&vector["expr"]).unwrap().paths() {
                let _ = path;
            }
            collect_tokens(&vector["expr"], &mut seen);
        }

        for token in operators() {
            assert!(
                seen.iter().any(|found| found == token),
                "no shared vector exercises `{token}`"
            );
        }
    }

    fn collect_tokens(value: &serde_json::Value, seen: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, inner) in map {
                    if key.starts_with('@') && !seen.iter().any(|found| found == key) {
                        seen.push(key.clone());
                    }
                    collect_tokens(inner, seen);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_tokens(item, seen);
                }
            }
            _ => {}
        }
    }
}
