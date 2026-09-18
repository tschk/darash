//! Safe filter expressions for structured extraction output.
//!
//! `--where` uses this small language to keep or drop rows and tables without
//! `eval` and without arbitrary code. The grammar, from lowest to highest
//! precedence, is:
//!
//! ```text
//! expr        := or
//! or          := and ( "||" and )*
//! and         := not ( "&&" not )*
//! not         := "!" not | comparison
//! comparison  := primary ( ( "==" | "!=" | "~" | "!~" | ">" | ">=" | "<" | "<=" ) primary )?
//! primary     := number | string | /regex/flags | true | false | `literal` | path | "(" expr ")"
//! ```
//!
//! Primaries are numbers, quoted strings, `/regex/[flags]`, booleans, backtick
//! literal column names, dotted ASCII identifier paths, and parentheses.
//! Comparisons coerce numeric strings, so `"10" > 2` holds. `~` and `!~` match
//! a string against a `/regex/` on the right. A dotted name first prefers a
//! literally named column (for example a header `foo.bar`) before traversing
//! nested values; the segment `length` resolves to a string or array length.
//!
//! The lexer and parser are hand written, and evaluation is a tree walk over
//! [`serde_json::Value`]. A compiled [`Filter`] can be reused across rows.

use regex::Regex;
use serde_json::{Number, Value};

/// A parse or evaluation failure for a `--where` expression.
#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    #[error("unexpected character {0:?} at position {1}")]
    UnexpectedChar(char, usize),
    #[error("unterminated {0}")]
    Unterminated(&'static str),
    #[error("unexpected token {0}")]
    UnexpectedToken(String),
    #[error("expected {0}")]
    Expected(&'static str),
    #[error("invalid regex: {0}")]
    Regex(String),
    #[error("`~` and `!~` require a /regex/ on the right-hand side")]
    NeedRegex,
    #[error("a /regex/ literal is only valid with `~` or `!~`")]
    StrayRegex,
    #[error("unknown regex flag {0:?}")]
    UnknownFlag(char),
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Or,
    And,
    Not,
    Eq,
    Ne,
    Match,
    NotMatch,
    Gt,
    Ge,
    Lt,
    Le,
    LParen,
    RParen,
    Number(f64),
    Str(String),
    Regex(String, String),
    Bool(bool),
    Literal(String),
    Path(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CmpOp {
    Eq,
    Ne,
    Match,
    NotMatch,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Clone, Debug)]
enum Expr {
    Number(f64),
    Str(String),
    Bool(bool),
    Regex(Regex),
    Path { raw: String, segments: Vec<String> },
    Literal(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Compare(CmpOp, Box<Expr>, Box<Expr>),
}

/// A compiled `--where` expression, reusable across rows.
pub struct Filter {
    expr: Expr,
}

impl Filter {
    /// Evaluate the expression and apply truthiness.
    pub fn matches(&self, value: &Value) -> bool {
        truthy(&eval(&self.expr, value))
    }

    /// Evaluate the expression to a JSON value.
    pub fn eval(&self, value: &Value) -> Value {
        eval(&self.expr, value)
    }
}

/// Compile a `--where` expression.
pub fn compile(expr: &str) -> Result<Filter, FilterError> {
    let tokens = tokenize(expr)?;
    if tokens.is_empty() {
        return Err(FilterError::Expected("an expression"));
    }
    let mut parser = Parser { tokens, pos: 0 };
    let ast = parser.parse_or()?;
    if parser.pos != parser.tokens.len() {
        return Err(FilterError::UnexpectedToken(token_name(
            &parser.tokens[parser.pos],
        )));
    }
    Ok(Filter { expr: ast })
}

/// Compile and evaluate a `--where` expression against one value.
pub fn evaluate(expr: &str, value: &Value) -> Result<bool, FilterError> {
    Ok(compile(expr)?.matches(value))
}

fn tokenize(input: &str) -> Result<Vec<Token>, FilterError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '|' => {
                if chars.get(i + 1) == Some(&'|') {
                    tokens.push(Token::Or);
                    i += 2;
                } else {
                    return Err(FilterError::UnexpectedChar(c, i));
                }
            }
            '&' => {
                if chars.get(i + 1) == Some(&'&') {
                    tokens.push(Token::And);
                    i += 2;
                } else {
                    return Err(FilterError::UnexpectedChar(c, i));
                }
            }
            '=' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push(Token::Eq);
                    i += 2;
                } else {
                    return Err(FilterError::UnexpectedChar(c, i));
                }
            }
            '!' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push(Token::Ne);
                    i += 2;
                } else if chars.get(i + 1) == Some(&'~') {
                    tokens.push(Token::NotMatch);
                    i += 2;
                } else {
                    tokens.push(Token::Not);
                    i += 1;
                }
            }
            '~' => {
                tokens.push(Token::Match);
                i += 1;
            }
            '>' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push(Token::Ge);
                    i += 2;
                } else {
                    tokens.push(Token::Gt);
                    i += 1;
                }
            }
            '<' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push(Token::Le);
                    i += 2;
                } else {
                    tokens.push(Token::Lt);
                    i += 1;
                }
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '"' | '\'' => {
                let (value, next) = lex_string(&chars, i, c)?;
                tokens.push(Token::Str(value));
                i = next;
            }
            '`' => {
                let (value, next) = lex_backtick(&chars, i)?;
                tokens.push(Token::Literal(value));
                i = next;
            }
            '/' => {
                let (pattern, flags, next) = lex_regex(&chars, i)?;
                tokens.push(Token::Regex(pattern, flags));
                i = next;
            }
            c if c.is_ascii_digit() => {
                let (number, next) = lex_number(&chars, i)?;
                tokens.push(Token::Number(number));
                i = next;
            }
            '-' if chars
                .get(i + 1)
                .map(|n| n.is_ascii_digit())
                .unwrap_or(false) =>
            {
                let (number, next) = lex_number(&chars, i)?;
                tokens.push(Token::Number(number));
                i = next;
            }
            c if is_ident_start(c) => {
                let (path, next) = lex_path(&chars, i)?;
                match path.as_str() {
                    "true" => tokens.push(Token::Bool(true)),
                    "false" => tokens.push(Token::Bool(false)),
                    _ => tokens.push(Token::Path(path)),
                }
                i = next;
            }
            _ => return Err(FilterError::UnexpectedChar(c, i)),
        }
    }
    Ok(tokens)
}

fn lex_string(chars: &[char], start: usize, quote: char) -> Result<(String, usize), FilterError> {
    let mut out = String::new();
    let mut i = start + 1;
    while i < chars.len() {
        let c = chars[i];
        if c == quote {
            return Ok((out, i + 1));
        }
        if c == '\\' {
            let Some(&next) = chars.get(i + 1) else {
                return Err(FilterError::Unterminated("string"));
            };
            match next {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '/' => out.push('/'),
                other => out.push(other),
            }
            i += 2;
        } else {
            out.push(c);
            i += 1;
        }
    }
    Err(FilterError::Unterminated("string"))
}

fn lex_backtick(chars: &[char], start: usize) -> Result<(String, usize), FilterError> {
    let mut out = String::new();
    let mut i = start + 1;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            return Ok((out, i + 1));
        }
        if c == '\\' && chars.get(i + 1) == Some(&'`') {
            out.push('`');
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    Err(FilterError::Unterminated("literal name"))
}

fn lex_regex(chars: &[char], start: usize) -> Result<(String, String, usize), FilterError> {
    let mut pattern = String::new();
    let mut i = start + 1;
    let mut closed = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            let Some(&next) = chars.get(i + 1) else {
                return Err(FilterError::Unterminated("regex"));
            };
            if next == '/' {
                pattern.push('/');
            } else {
                pattern.push('\\');
                pattern.push(next);
            }
            i += 2;
            continue;
        }
        if c == '/' {
            closed = true;
            i += 1;
            break;
        }
        pattern.push(c);
        i += 1;
    }
    if !closed {
        return Err(FilterError::Unterminated("regex"));
    }
    let mut flags = String::new();
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        let flag = chars[i];
        if !matches!(flag, 'i' | 'm' | 's' | 'U' | 'x') {
            return Err(FilterError::UnknownFlag(flag));
        }
        flags.push(flag);
        i += 1;
    }
    Ok((pattern, flags, i))
}

fn lex_number(chars: &[char], start: usize) -> Result<(f64, usize), FilterError> {
    let mut text = String::new();
    let mut i = start;
    if chars[i] == '-' {
        text.push('-');
        i += 1;
    }
    while i < chars.len() && chars[i].is_ascii_digit() {
        text.push(chars[i]);
        i += 1;
    }
    if i < chars.len()
        && chars[i] == '.'
        && chars
            .get(i + 1)
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    {
        text.push('.');
        i += 1;
        while i < chars.len() && chars[i].is_ascii_digit() {
            text.push(chars[i]);
            i += 1;
        }
    }
    text.parse::<f64>()
        .map(|number| (number, i))
        .map_err(|_| FilterError::UnexpectedToken(text))
}

fn lex_path(chars: &[char], start: usize) -> Result<(String, usize), FilterError> {
    let mut out = String::new();
    let mut i = start;
    while i < chars.len() && is_ident_continue(chars[i]) {
        out.push(chars[i]);
        i += 1;
    }
    loop {
        if i + 1 < chars.len() && chars[i] == '.' && is_ident_start(chars[i + 1]) {
            out.push('.');
            i += 1;
            while i < chars.len() && is_ident_continue(chars[i]) {
                out.push(chars[i]);
                i += 1;
            }
        } else {
            break;
        }
    }
    Ok((out, i))
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    fn parse_or(&mut self) -> Result<Expr, FilterError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.pos += 1;
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, FilterError> {
        let mut left = self.parse_not()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.pos += 1;
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, FilterError> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.pos += 1;
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, FilterError> {
        let left = self.parse_primary()?;
        let op = match self.peek() {
            Some(Token::Eq) => Some(CmpOp::Eq),
            Some(Token::Ne) => Some(CmpOp::Ne),
            Some(Token::Match) => Some(CmpOp::Match),
            Some(Token::NotMatch) => Some(CmpOp::NotMatch),
            Some(Token::Gt) => Some(CmpOp::Gt),
            Some(Token::Ge) => Some(CmpOp::Ge),
            Some(Token::Lt) => Some(CmpOp::Lt),
            Some(Token::Le) => Some(CmpOp::Le),
            _ => None,
        };
        let Some(op) = op else {
            if matches!(left, Expr::Regex(_)) {
                return Err(FilterError::StrayRegex);
            }
            return Ok(left);
        };
        self.pos += 1;
        let right = self.parse_primary()?;
        let left_is_regex = matches!(left, Expr::Regex(_));
        let right_is_regex = matches!(right, Expr::Regex(_));
        if matches!(op, CmpOp::Match | CmpOp::NotMatch) {
            if !right_is_regex {
                return Err(FilterError::NeedRegex);
            }
            if left_is_regex {
                return Err(FilterError::StrayRegex);
            }
        } else if left_is_regex || right_is_regex {
            return Err(FilterError::StrayRegex);
        }
        Ok(Expr::Compare(op, Box::new(left), Box::new(right)))
    }

    fn parse_primary(&mut self) -> Result<Expr, FilterError> {
        match self.next() {
            Some(Token::Number(number)) => Ok(Expr::Number(number)),
            Some(Token::Str(value)) => Ok(Expr::Str(value)),
            Some(Token::Bool(value)) => Ok(Expr::Bool(value)),
            Some(Token::Regex(pattern, flags)) => Ok(Expr::Regex(build_regex(&pattern, &flags)?)),
            Some(Token::Literal(name)) => Ok(Expr::Literal(name)),
            Some(Token::Path(raw)) => {
                let segments = raw.split('.').map(str::to_owned).collect();
                Ok(Expr::Path { raw, segments })
            }
            Some(Token::LParen) => {
                let inner = self.parse_or()?;
                match self.next() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(FilterError::Expected("`)`")),
                }
            }
            Some(token) => Err(FilterError::UnexpectedToken(token_name(&token))),
            None => Err(FilterError::Expected("an expression")),
        }
    }
}

fn build_regex(pattern: &str, flags: &str) -> Result<Regex, FilterError> {
    let full = if flags.is_empty() {
        pattern.to_owned()
    } else {
        format!("(?{flags}){pattern}")
    };
    Regex::new(&full).map_err(|error| FilterError::Regex(error.to_string()))
}

fn eval(expr: &Expr, root: &Value) -> Value {
    match expr {
        Expr::Number(number) => Number::from_f64(*number)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Expr::Str(value) => Value::String(value.clone()),
        Expr::Bool(value) => Value::Bool(*value),
        Expr::Regex(_) => Value::Null,
        Expr::Literal(name) => resolve_key(root, name),
        Expr::Path { raw, segments } => resolve_path(root, raw, segments),
        Expr::Not(inner) => Value::Bool(!truthy(&eval(inner, root))),
        Expr::And(left, right) => {
            Value::Bool(truthy(&eval(left, root)) && truthy(&eval(right, root)))
        }
        Expr::Or(left, right) => {
            Value::Bool(truthy(&eval(left, root)) || truthy(&eval(right, root)))
        }
        Expr::Compare(op, left, right) => Value::Bool(compare(*op, left, right, root)),
    }
}

fn resolve_key(root: &Value, key: &str) -> Value {
    root.get(key).cloned().unwrap_or(Value::Null)
}

fn resolve_path(root: &Value, raw: &str, segments: &[String]) -> Value {
    // A literally named column wins over nested traversal.
    if let Some(value) = root.get(raw) {
        return value.clone();
    }
    let mut current = root.clone();
    for segment in segments {
        current = step(&current, segment);
        if current.is_null() {
            break;
        }
    }
    current
}

fn step(current: &Value, segment: &str) -> Value {
    match current {
        Value::Object(map) => map.get(segment).cloned().unwrap_or(Value::Null),
        Value::Array(items) => {
            if segment == "length" {
                return Value::from(items.len());
            }
            match segment.parse::<usize>() {
                Ok(index) => items.get(index).cloned().unwrap_or(Value::Null),
                Err(_) => Value::Null,
            }
        }
        Value::String(value) => {
            if segment == "length" {
                return Value::from(value.chars().count());
            }
            Value::Null
        }
        _ => Value::Null,
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().map(|value| value != 0.0).unwrap_or(false),
        Value::String(value) => !value.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn to_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn compare(op: CmpOp, left: &Expr, right: &Expr, root: &Value) -> bool {
    let left_value = eval(left, root);
    match op {
        CmpOp::Match | CmpOp::NotMatch => {
            let Expr::Regex(regex) = right else {
                return false;
            };
            let matched = regex.is_match(&to_string(&left_value));
            if op == CmpOp::NotMatch {
                !matched
            } else {
                matched
            }
        }
        CmpOp::Eq => loose_eq(&left_value, &eval(right, root)),
        CmpOp::Ne => !loose_eq(&left_value, &eval(right, root)),
        CmpOp::Gt | CmpOp::Ge | CmpOp::Lt | CmpOp::Le => {
            ordered(op, &left_value, &eval(right, root))
        }
    }
}

fn loose_eq(left: &Value, right: &Value) -> bool {
    if let (Some(left), Some(right)) = (to_number(left), to_number(right)) {
        return left == right;
    }
    to_string(left) == to_string(right)
}

fn ordered(op: CmpOp, left: &Value, right: &Value) -> bool {
    if let (Some(left), Some(right)) = (to_number(left), to_number(right)) {
        return match op {
            CmpOp::Gt => left > right,
            CmpOp::Ge => left >= right,
            CmpOp::Lt => left < right,
            CmpOp::Le => left <= right,
            _ => false,
        };
    }
    let left = to_string(left);
    let right = to_string(right);
    match op {
        CmpOp::Gt => left > right,
        CmpOp::Ge => left >= right,
        CmpOp::Lt => left < right,
        CmpOp::Le => left <= right,
        _ => false,
    }
}

fn token_name(token: &Token) -> String {
    match token {
        Token::Or => "`||`".to_owned(),
        Token::And => "`&&`".to_owned(),
        Token::Not => "`!`".to_owned(),
        Token::Eq => "`==`".to_owned(),
        Token::Ne => "`!=`".to_owned(),
        Token::Match => "`~`".to_owned(),
        Token::NotMatch => "`!~`".to_owned(),
        Token::Gt => "`>`".to_owned(),
        Token::Ge => "`>=`".to_owned(),
        Token::Lt => "`<`".to_owned(),
        Token::Le => "`<=`".to_owned(),
        Token::LParen => "`(`".to_owned(),
        Token::RParen => "`)`".to_owned(),
        Token::Number(number) => format!("number {number}"),
        Token::Str(value) => format!("string {value:?}"),
        Token::Regex(pattern, flags) => format!("/{pattern}/{flags}"),
        Token::Bool(value) => value.to_string(),
        Token::Literal(name) => format!("`{name}`"),
        Token::Path(path) => path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row() -> Value {
        json!({
            "title": "Rust Async",
            "price": "10",
            "tags": ["a", "b", "c"],
            "author": { "name": "Ada" },
            "foo.bar": "literal"
        })
    }

    #[test]
    fn numeric_strings_coerce_in_comparisons() {
        let value = row();
        assert!(evaluate("price > 2", &value).expect("valid"));
        assert!(evaluate("price == 10", &value).expect("valid"));
        assert!(!evaluate("price < 10", &value).expect("valid"));
        assert!(evaluate("price >= 10", &value).expect("valid"));
    }

    #[test]
    fn string_and_boolean_comparisons() {
        let value = row();
        assert!(evaluate("title == \"Rust Async\"", &value).expect("valid"));
        assert!(evaluate("title != 'other'", &value).expect("valid"));
        assert!(evaluate("true", &value).expect("valid"));
        assert!(!evaluate("false", &value).expect("valid"));
    }

    #[test]
    fn regex_match_and_negation() {
        let value = row();
        assert!(evaluate("title ~ /rust/i", &value).expect("valid"));
        assert!(evaluate("title !~ /python/", &value).expect("valid"));
        assert!(!evaluate("title ~ /python/", &value).expect("valid"));
        assert!(evaluate("price ~ /^\\d+$/", &value).expect("valid"));
    }

    #[test]
    fn regex_on_the_right_is_required() {
        let value = row();
        let error = evaluate("title ~ \"rust\"", &value).expect_err("string rhs is rejected");
        assert!(matches!(error, FilterError::NeedRegex));
        let error = evaluate("/rust/ == title", &value).expect_err("stray regex is rejected");
        assert!(matches!(error, FilterError::StrayRegex));
    }

    #[test]
    fn boolean_precedence_and_parentheses() {
        let value = row();
        assert!(evaluate("price > 100 || title ~ /rust/i", &value).expect("valid"));
        assert!(!evaluate("price > 100 && title ~ /rust/i", &value).expect("valid"));
        assert!(evaluate("!(price > 100)", &value).expect("valid"));
        assert!(evaluate("(price > 100 || true) && true", &value).expect("valid"));
    }

    #[test]
    fn dotted_paths_prefer_literal_columns_and_resolve_length() {
        let value = row();
        // A literal column named `foo.bar` wins over traversal.
        assert!(evaluate("foo.bar == \"literal\"", &value).expect("valid"));
        // Nested traversal when no literal column exists.
        assert!(evaluate("author.name == \"Ada\"", &value).expect("valid"));
        // Backtick literal names.
        assert!(evaluate("`foo.bar` == \"literal\"", &value).expect("valid"));
        // `length` resolves to string/array length.
        assert!(evaluate("title.length == 10", &value).expect("valid"));
        assert!(evaluate("tags.length == 3", &value).expect("valid"));
    }

    #[test]
    fn missing_paths_are_null_and_falsy() {
        let value = row();
        assert!(!evaluate("missing", &value).expect("valid"));
        assert!(evaluate("missing == \"\"", &value).expect("valid"));
    }

    #[test]
    fn invalid_expressions_report_errors() {
        assert!(compile("").is_err());
        assert!(compile("title ~").is_err());
        assert!(compile("title @").is_err());
        assert!(compile("(title == \"x\"").is_err());
    }
}
