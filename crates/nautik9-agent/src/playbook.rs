//! Playbooks: deterministic automations written as JSON, run through the agent's tools.
//!
//! The design follows Jade Vault 2.18 (Ansible-shaped playbooks, MIT upstream, intent only; no
//! Jade code is used). A playbook is a vault note `Jade/Playbooks/<name>.json`:
//!
//! ```json
//! {"name": "daily", "description": "...", "vars": {"folder": "Logs"},
//!  "tasks": [
//!    {"name": "write", "do": "write", "args": {"path": "{{ folder }}/{{ item }}.md", "content": "hi"},
//!     "loop": ["a", "b"], "register": "w", "when": "length(folder) > 0", "tags": ["x"],
//!     "timeout_s": 30, "ignore_errors": false},
//!    {"do": "assert", "args": {"that": "w.ok"}}]}
//! ```
//!
//! Task actions (`do`): any tool of the agent's registry (except `playbook` and `delegate`), or
//! `set` (args: name -> value), `fail` (args.msg), `assert` (args.that: expression or list of
//! them, args.msg), `ask` (args.prompt: one agent turn, returns its text) and `block` (the task
//! has `tasks`). A block takes `when`, `tags`, `ignore_errors`; it has no `loop` or `register`.
//!
//! ## Expression language (no arbitrary code)
//!
//! ```text
//! expr    := or
//! or      := and ("or" and)*
//! and     := not ("and" not)*
//! not     := "not" not | cmp
//! cmp     := add ( ("==" | "!=" | "<" | ">" | "<=" | ">=" | "in" | "not" "in"
//!                   | "contains" | "startswith" | "endswith") add )*
//! add     := unary (("+" | "-") unary)*
//! unary   := "-" unary | postfix
//! postfix := primary ("." name | "[" expr "]")*
//! primary := number | 'string' | "string" | true | false | null | name
//!          | name "(" [expr ("," expr)*] ")" | "[" [expr ("," expr)*] "]" | "(" expr ")"
//! ```
//!
//! Functions: `length`, `lower`, `upper`, `trim`, `str`, `int`, `json` (parse a JSON string),
//! `keys`, `matches(text, pattern)` (guarded regex), `defined(x)` (the only way to look at a name
//! that may not exist). A name that is not declared is an ERROR, never an empty string. Names in
//! scope: the playbook's `vars` (a run may only override declared ones), names created by `set`,
//! registered results (`{ok, result, error, skipped?}`; a looped task registers
//! `{ok, result (last), error, results: [{ok, result, error, item}]}`) and, inside a loop, `item`
//! and `index`. Text containing `{{ expr }}` is templated; a string that is exactly one
//! `{{ expr }}` keeps the value's type. `when` is a boolean, or an expression whose truthiness
//! (null, false, 0, "" and empty lists are false) decides.
//!
//! ## Safety
//!
//! Expressions are length-limited (4000 chars), nesting-limited and step-budgeted; the regex
//! helper uses the linear-time `regex` engine with a pattern size cap, a compiled-size limit and
//! a haystack cap, so a catastrophic pattern cannot hang a run. Loops over more than 200 items
//! are an error, not a truncation. A tool call is cut off after the task's `timeout_s` (default
//! 120, at most 3600). Problems in `when`, `loop` or templating are fatal and cannot be hidden
//! with `ignore_errors` (which covers failures of the action only).
//!
//! ## Approval
//!
//! `approve` stores the SHA-256 of the playbook's canonical JSON (keys sorted, whitespace
//! ignored) in the sessions DB. A run with trigger `schedule` needs a matching approval; any
//! change to the JSON voids it. Manual runs by the owner need none and are logged as manual.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Weak;
use std::time::Duration;

use crate::agent::Agent;
use crate::tools::{note_by_path, Registry, Tool, ToolCtx};

pub const PLAYBOOK_DIR: &str = "Jade/Playbooks/";
pub const MAX_LOOP: usize = 200;
const MAX_EXPR_CHARS: usize = 4000;
const MAX_DEPTH: usize = 48;
const MAX_STEPS: u32 = 50_000;
const MAX_STRING: usize = 1_000_000;
const MAX_REGEX_PATTERN: usize = 200;
const MAX_REGEX_HAYSTACK: usize = 100_000;
const DEFAULT_TIMEOUT_S: u64 = 120;
const MAX_TIMEOUT_S: u64 = 3600;
const RESERVED: &[&str] = &["item", "index", "true", "false", "null", "and", "or", "not", "in", "contains", "startswith", "endswith"];
/// Tools a playbook may never call (no recursion, no hidden model loop).
const FORBIDDEN_TOOLS: &[&str] = &["playbook", "delegate"];

// ── expressions ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ExprError {
    /// A name that is not declared in scope.
    Undeclared(String),
    Msg(String),
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExprError::Undeclared(n) => write!(f, "undeclared variable `{n}`"),
            ExprError::Msg(m) => write!(f, "{m}"),
        }
    }
}

type R<T> = Result<T, ExprError>;

fn err<T>(m: impl Into<String>) -> R<T> {
    Err(ExprError::Msg(m.into()))
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(Value),
    Str(String),
    Id(String),
    Op(&'static str),
}

fn lex(src: &str) -> R<Vec<Tok>> {
    if src.chars().count() > MAX_EXPR_CHARS {
        return err(format!("expression longer than {MAX_EXPR_CHARS} characters"));
    }
    let c: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_ascii_digit() {
            let st = i;
            while i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            }
            let mut float = false;
            if i + 1 < c.len() && c[i] == '.' && c[i + 1].is_ascii_digit() {
                float = true;
                i += 1;
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let t: String = c[st..i].iter().collect();
            let v = if float { t.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number) } else { t.parse::<i64>().ok().map(Value::from) };
            out.push(Tok::Num(v.ok_or_else(|| ExprError::Msg(format!("bad number `{t}`")))?));
        } else if ch.is_ascii_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(c[st..i].iter().collect()));
        } else if ch == '\'' || ch == '"' {
            let q = ch;
            i += 1;
            let mut s = String::new();
            loop {
                if i >= c.len() {
                    return err("unterminated string");
                }
                let d = c[i];
                i += 1;
                if d == q {
                    break;
                }
                if d == '\\' {
                    let e = *c.get(i).ok_or_else(|| ExprError::Msg("unterminated string".into()))?;
                    i += 1;
                    s.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                } else {
                    s.push(d);
                }
            }
            out.push(Tok::Str(s));
        } else {
            let two: String = c[i..(i + 2).min(c.len())].iter().collect();
            let op = match two.as_str() {
                "==" => Some("=="),
                "!=" => Some("!="),
                "<=" => Some("<="),
                ">=" => Some(">="),
                _ => None,
            };
            if let Some(o) = op {
                out.push(Tok::Op(o));
                i += 2;
                continue;
            }
            let o = match ch {
                '<' => "<",
                '>' => ">",
                '+' => "+",
                '-' => "-",
                '(' => "(",
                ')' => ")",
                '[' => "[",
                ']' => "]",
                ',' => ",",
                '.' => ".",
                other => return err(format!("unexpected character `{other}`")),
            };
            out.push(Tok::Op(o));
            i += 1;
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bin {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    In,
    NotIn,
    Contains,
    StartsWith,
    EndsWith,
    Add,
    Sub,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Lit(Value),
    Var(String),
    List(Vec<Expr>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Bin(Bin, Box<Expr>, Box<Expr>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n)
    }
    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Id(x)) if x == w)
    }
    fn is_op(&self, o: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(x)) if *x == o)
    }
    fn expect_op(&mut self, o: &str) -> R<()> {
        if self.is_op(o) {
            self.pos += 1;
            Ok(())
        } else {
            err(format!("expected `{o}`{}", self.near()))
        }
    }
    fn near(&self) -> String {
        match self.peek() {
            Some(Tok::Id(x)) => format!(" near `{x}`"),
            Some(Tok::Op(x)) => format!(" near `{x}`"),
            Some(Tok::Str(x)) => format!(" near '{x}'"),
            Some(Tok::Num(x)) => format!(" near {x}"),
            None => " at the end".into(),
        }
    }
    fn enter(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return err("expression nested too deeply");
        }
        Ok(())
    }
    fn or(&mut self) -> R<Expr> {
        self.enter()?;
        let mut l = self.and()?;
        while self.is_word("or") {
            self.pos += 1;
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        self.depth -= 1;
        Ok(l)
    }
    fn and(&mut self) -> R<Expr> {
        let mut l = self.not()?;
        while self.is_word("and") {
            self.pos += 1;
            l = Expr::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> R<Expr> {
        if self.is_word("not") {
            self.pos += 1;
            self.enter()?;
            let e = self.not()?;
            self.depth -= 1;
            return Ok(Expr::Not(Box::new(e)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> R<Expr> {
        let mut l = self.add()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op("==")) => Bin::Eq,
                Some(Tok::Op("!=")) => Bin::Ne,
                Some(Tok::Op("<")) => Bin::Lt,
                Some(Tok::Op(">")) => Bin::Gt,
                Some(Tok::Op("<=")) => Bin::Le,
                Some(Tok::Op(">=")) => Bin::Ge,
                Some(Tok::Id(w)) if w == "in" => Bin::In,
                Some(Tok::Id(w)) if w == "contains" => Bin::Contains,
                Some(Tok::Id(w)) if w == "startswith" => Bin::StartsWith,
                Some(Tok::Id(w)) if w == "endswith" => Bin::EndsWith,
                Some(Tok::Id(w)) if w == "not" && matches!(self.peek_at(1), Some(Tok::Id(x)) if x == "in") => {
                    self.pos += 1;
                    Bin::NotIn
                }
                _ => return Ok(l),
            };
            self.pos += 1;
            l = Expr::Bin(op, Box::new(l), Box::new(self.add()?));
        }
    }
    fn add(&mut self) -> R<Expr> {
        let mut l = self.unary()?;
        loop {
            let op = if self.is_op("+") {
                Bin::Add
            } else if self.is_op("-") {
                Bin::Sub
            } else {
                return Ok(l);
            };
            self.pos += 1;
            l = Expr::Bin(op, Box::new(l), Box::new(self.unary()?));
        }
    }
    fn unary(&mut self) -> R<Expr> {
        if self.is_op("-") {
            self.pos += 1;
            self.enter()?;
            let e = self.unary()?;
            self.depth -= 1;
            return Ok(Expr::Neg(Box::new(e)));
        }
        self.postfix()
    }
    fn postfix(&mut self) -> R<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.is_op(".") {
                self.pos += 1;
                match self.peek().cloned() {
                    Some(Tok::Id(n)) => {
                        self.pos += 1;
                        e = Expr::Field(Box::new(e), n);
                    }
                    _ => return err(format!("expected a field name after `.`{}", self.near())),
                }
            } else if self.is_op("[") {
                self.pos += 1;
                self.enter()?;
                let i = self.or()?;
                self.depth -= 1;
                self.expect_op("]")?;
                e = Expr::Index(Box::new(e), Box::new(i));
            } else {
                return Ok(e);
            }
        }
    }
    fn args(&mut self, close: &str) -> R<Vec<Expr>> {
        let mut v = Vec::new();
        if self.is_op(close) {
            self.pos += 1;
            return Ok(v);
        }
        loop {
            self.enter()?;
            v.push(self.or()?);
            self.depth -= 1;
            if self.is_op(",") {
                self.pos += 1;
            } else {
                self.expect_op(close)?;
                return Ok(v);
            }
        }
    }
    fn primary(&mut self) -> R<Expr> {
        let t = self.peek().cloned().ok_or_else(|| ExprError::Msg("expression ended early".into()))?;
        self.pos += 1;
        match t {
            Tok::Num(v) => Ok(Expr::Lit(v)),
            Tok::Str(s) => Ok(Expr::Lit(Value::String(s))),
            Tok::Id(n) => match n.as_str() {
                "true" => Ok(Expr::Lit(Value::Bool(true))),
                "false" => Ok(Expr::Lit(Value::Bool(false))),
                "null" => Ok(Expr::Lit(Value::Null)),
                "and" | "or" | "not" | "in" | "contains" | "startswith" | "endswith" => {
                    self.pos -= 1;
                    err(format!("unexpected word `{n}`{}", self.near()))
                }
                _ => {
                    if self.is_op("(") {
                        self.pos += 1;
                        let a = self.args(")")?;
                        Ok(Expr::Call(n, a))
                    } else {
                        Ok(Expr::Var(n))
                    }
                }
            },
            Tok::Op("(") => {
                self.enter()?;
                let e = self.or()?;
                self.depth -= 1;
                self.expect_op(")")?;
                Ok(e)
            }
            Tok::Op("[") => Ok(Expr::List(self.args("]")?)),
            Tok::Op(o) => {
                self.pos -= 1;
                err(format!("unexpected `{o}`"))
            }
        }
    }
}

/// Parse one expression (the whole string must be consumed).
pub fn parse_expr(src: &str) -> R<Expr> {
    let toks = lex(src)?;
    if toks.is_empty() {
        return err("empty expression");
    }
    let mut p = Parser { toks, pos: 0, depth: 0 };
    let e = p.or()?;
    if p.pos != p.toks.len() {
        return err(format!("unexpected input{}", p.near()));
    }
    Ok(e)
}

impl Expr {
    /// Names the expression needs in scope (`defined(x)` does not need `x`).
    pub fn names(&self, out: &mut Vec<String>) {
        match self {
            Expr::Lit(_) => {}
            Expr::Var(n) => out.push(n.clone()),
            Expr::List(v) => v.iter().for_each(|e| e.names(out)),
            Expr::Field(e, _) | Expr::Not(e) | Expr::Neg(e) => e.names(out),
            Expr::Index(a, b) | Expr::And(a, b) | Expr::Or(a, b) | Expr::Bin(_, a, b) => {
                a.names(out);
                b.names(out);
            }
            Expr::Call(f, a) => {
                if f != "defined" {
                    a.iter().for_each(|e| e.names(out));
                }
            }
        }
    }
}

pub type Scope = HashMap<String, Value>;

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn from_f64(f: f64) -> R<Value> {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        Ok(Value::from(f as i64))
    } else {
        serde_json::Number::from_f64(f).map(Value::Number).ok_or_else(|| ExprError::Msg("number is not finite".into()))
    }
}

pub fn to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "list",
        Value::Object(_) => "object",
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| values_equal(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| values_equal(v, w))),
        _ => a == b,
    }
}

fn contains(hay: &Value, needle: &Value) -> R<bool> {
    match hay {
        Value::String(h) => match needle {
            Value::String(n) => Ok(h.contains(n.as_str())),
            other => err(format!("cannot look for a {} inside text", kind(other))),
        },
        Value::Array(a) => Ok(a.iter().any(|x| values_equal(x, needle))),
        Value::Object(o) => match needle {
            Value::String(n) => Ok(o.contains_key(n)),
            other => err(format!("object keys are text, not {}", kind(other))),
        },
        other => err(format!("`contains`/`in` need text, a list or an object, not {}", kind(other))),
    }
}

/// Regex helper: linear-time engine, pattern and haystack caps.
pub fn guarded_regex_is_match(text: &str, pattern: &str) -> R<bool> {
    if pattern.chars().count() > MAX_REGEX_PATTERN {
        return err(format!("regex longer than {MAX_REGEX_PATTERN} characters"));
    }
    if text.len() > MAX_REGEX_HAYSTACK {
        return err(format!("text longer than {MAX_REGEX_HAYSTACK} bytes for a regex"));
    }
    let re = regex::RegexBuilder::new(pattern)
        .size_limit(200_000)
        .dfa_size_limit(1_000_000)
        .nest_limit(20)
        .build()
        .map_err(|e| ExprError::Msg(format!("regex rejected: {}", e.to_string().lines().last().unwrap_or("invalid"))))?;
    Ok(re.is_match(text))
}

struct Ev<'a> {
    scope: &'a Scope,
    steps: u32,
}

impl Ev<'_> {
    fn eval(&mut self, e: &Expr) -> R<Value> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return err("expression too expensive");
        }
        match e {
            Expr::Lit(v) => Ok(v.clone()),
            Expr::Var(n) => self.scope.get(n).cloned().ok_or_else(|| ExprError::Undeclared(n.clone())),
            Expr::List(v) => Ok(Value::Array(v.iter().map(|x| self.eval(x)).collect::<R<_>>()?)),
            Expr::Field(b, f) => match self.eval(b)? {
                Value::Object(o) => o.get(f).cloned().ok_or_else(|| ExprError::Msg(format!("no field `{f}`"))),
                other => err(format!("cannot read `.{f}` of a {}", kind(&other))),
            },
            Expr::Index(b, i) => {
                let (b, i) = (self.eval(b)?, self.eval(i)?);
                match (&b, &i) {
                    (Value::Array(a), Value::Number(n)) => {
                        let idx = n.as_i64().ok_or_else(|| ExprError::Msg("list index must be a whole number".into()))?;
                        let real = if idx < 0 { a.len() as i64 + idx } else { idx };
                        a.get(real as usize).filter(|_| real >= 0).cloned().ok_or_else(|| ExprError::Msg(format!("list index {idx} out of range (length {})", a.len())))
                    }
                    (Value::Object(o), Value::String(k)) => o.get(k).cloned().ok_or_else(|| ExprError::Msg(format!("no key `{k}`"))),
                    _ => err(format!("cannot index a {} with a {}", kind(&b), kind(&i))),
                }
            }
            Expr::Not(x) => Ok(Value::Bool(!truthy(&self.eval(x)?))),
            Expr::Neg(x) => {
                let v = self.eval(x)?;
                from_f64(-num(&v).ok_or_else(|| ExprError::Msg(format!("cannot negate a {}", kind(&v))))?)
            }
            Expr::And(a, b) => Ok(Value::Bool(truthy(&self.eval(a)?) && truthy(&self.eval(b)?))),
            Expr::Or(a, b) => Ok(Value::Bool(truthy(&self.eval(a)?) || truthy(&self.eval(b)?))),
            Expr::Bin(op, a, b) => {
                let (x, y) = (self.eval(a)?, self.eval(b)?);
                self.bin(*op, &x, &y)
            }
            Expr::Call(f, args) => self.call(f, args),
        }
    }

    fn bin(&mut self, op: Bin, x: &Value, y: &Value) -> R<Value> {
        let b = |v: bool| Ok(Value::Bool(v));
        match op {
            Bin::Eq => b(values_equal(x, y)),
            Bin::Ne => b(!values_equal(x, y)),
            Bin::Lt | Bin::Gt | Bin::Le | Bin::Ge => {
                let ord = match (x, y) {
                    (Value::Number(_), Value::Number(_)) => num(x).partial_cmp(&num(y)),
                    (Value::String(p), Value::String(q)) => Some(p.cmp(q)),
                    _ => return err(format!("cannot order a {} and a {}", kind(x), kind(y))),
                };
                let ord = ord.ok_or_else(|| ExprError::Msg("cannot compare these numbers".into()))?;
                b(match op {
                    Bin::Lt => ord.is_lt(),
                    Bin::Gt => ord.is_gt(),
                    Bin::Le => ord.is_le(),
                    _ => ord.is_ge(),
                })
            }
            Bin::In => b(contains(y, x)?),
            Bin::NotIn => b(!contains(y, x)?),
            Bin::Contains => b(contains(x, y)?),
            Bin::StartsWith | Bin::EndsWith => match (x, y) {
                (Value::String(p), Value::String(q)) => b(if op == Bin::StartsWith { p.starts_with(q.as_str()) } else { p.ends_with(q.as_str()) }),
                _ => err(format!("startswith/endswith need two texts, got {} and {}", kind(x), kind(y))),
            },
            Bin::Add => match (x, y) {
                (Value::Number(_), Value::Number(_)) => from_f64(num(x).unwrap() + num(y).unwrap()),
                (Value::String(p), Value::String(q)) => {
                    if p.len() + q.len() > MAX_STRING {
                        return err("text too long");
                    }
                    Ok(Value::String(format!("{p}{q}")))
                }
                (Value::Array(p), Value::Array(q)) => Ok(Value::Array(p.iter().chain(q).cloned().collect())),
                _ => err(format!("cannot add a {} and a {} (use str() to join text)", kind(x), kind(y))),
            },
            Bin::Sub => match (num(x), num(y)) {
                (Some(p), Some(q)) => from_f64(p - q),
                _ => err(format!("cannot subtract a {} and a {}", kind(x), kind(y))),
            },
        }
    }

    fn call(&mut self, f: &str, args: &[Expr]) -> R<Value> {
        if f == "defined" {
            if args.len() != 1 {
                return err("defined() takes one argument");
            }
            return match self.eval(&args[0]) {
                Ok(_) => Ok(Value::Bool(true)),
                Err(ExprError::Undeclared(_)) => Ok(Value::Bool(false)),
                Err(e) => Err(e),
            };
        }
        let vals: Vec<Value> = args.iter().map(|a| self.eval(a)).collect::<R<_>>()?;
        let want = |n: usize| if vals.len() == n { Ok(()) } else { err(format!("{f}() takes {n} argument(s), got {}", vals.len())) };
        match f {
            "length" => {
                want(1)?;
                match &vals[0] {
                    Value::String(s) => Ok(Value::from(s.chars().count())),
                    Value::Array(a) => Ok(Value::from(a.len())),
                    Value::Object(o) => Ok(Value::from(o.len())),
                    other => err(format!("length() of a {}", kind(other))),
                }
            }
            "lower" | "upper" | "trim" => {
                want(1)?;
                match &vals[0] {
                    Value::String(s) => Ok(Value::String(match f {
                        "lower" => s.to_lowercase(),
                        "upper" => s.to_uppercase(),
                        _ => s.trim().to_string(),
                    })),
                    other => err(format!("{f}() needs text, got {}", kind(other))),
                }
            }
            "str" => {
                want(1)?;
                Ok(Value::String(to_text(&vals[0])))
            }
            "int" => {
                want(1)?;
                match &vals[0] {
                    Value::Number(n) => from_f64(n.as_f64().unwrap_or(0.0).trunc()),
                    Value::String(s) => s.trim().parse::<i64>().map(Value::from).or_else(|_| s.trim().parse::<f64>().map_err(|_| ()).and_then(|f| from_f64(f.trunc()).map_err(|_| ()))).map_err(|_| ExprError::Msg(format!("int(): `{s}` is not a number"))),
                    Value::Bool(b) => Ok(Value::from(*b as i64)),
                    other => err(format!("int() of a {}", kind(other))),
                }
            }
            "json" => {
                want(1)?;
                match &vals[0] {
                    Value::String(s) if s.len() <= MAX_STRING => serde_json::from_str(s).map_err(|e| ExprError::Msg(format!("json(): {e}"))),
                    _ => err("json() needs a text of reasonable size"),
                }
            }
            "keys" => {
                want(1)?;
                match &vals[0] {
                    Value::Object(o) => Ok(Value::Array(o.keys().map(|k| Value::String(k.clone())).collect())),
                    other => err(format!("keys() of a {}", kind(other))),
                }
            }
            "matches" => {
                want(2)?;
                match (&vals[0], &vals[1]) {
                    (Value::String(t), Value::String(p)) => guarded_regex_is_match(t, p).map(Value::Bool),
                    _ => err("matches(text, pattern) needs two texts"),
                }
            }
            other => err(format!("unknown function `{other}()`")),
        }
    }
}

/// Evaluate a parsed expression in `scope`.
pub fn eval(e: &Expr, scope: &Scope) -> R<Value> {
    Ev { scope, steps: 0 }.eval(e)
}

/// Parse and evaluate.
pub fn eval_str(src: &str, scope: &Scope) -> R<Value> {
    eval(&parse_expr(src)?, scope)
}

// ── templates ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Seg {
    Text(String),
    Expr(Expr),
}

/// Split text into literal parts and `{{ expr }}` parts (quotes inside an expression may hold `}}`).
pub fn parse_template(s: &str) -> R<Vec<Seg>> {
    let mut segs = Vec::new();
    let mut rest = s;
    while let Some(st) = rest.find("{{") {
        if st > 0 {
            segs.push(Seg::Text(rest[..st].to_string()));
        }
        let body = &rest[st + 2..];
        let (mut quote, mut esc, mut end) = (None::<char>, false, None);
        for (i, ch) in body.char_indices() {
            if let Some(q) = quote {
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == q {
                    quote = None;
                }
            } else if ch == '\'' || ch == '"' {
                quote = Some(ch);
            } else if body[i..].starts_with("}}") {
                end = Some(i);
                break;
            }
        }
        let end = end.ok_or_else(|| ExprError::Msg("`{{` without a closing `}}`".into()))?;
        segs.push(Seg::Expr(parse_expr(&body[..end])?));
        rest = &body[end + 2..];
    }
    if !rest.is_empty() {
        segs.push(Seg::Text(rest.to_string()));
    }
    Ok(segs)
}

/// Render a template string; one lone `{{ expr }}` keeps the value's type.
pub fn render_str(s: &str, scope: &Scope) -> R<Value> {
    if !s.contains("{{") {
        return Ok(Value::String(s.to_string()));
    }
    let segs = parse_template(s)?;
    if segs.len() == 1 {
        if let Seg::Expr(e) = &segs[0] {
            return eval(e, scope);
        }
    }
    let mut out = String::new();
    for seg in &segs {
        match seg {
            Seg::Text(t) => out.push_str(t),
            Seg::Expr(e) => out.push_str(&to_text(&eval(e, scope)?)),
        }
        if out.len() > MAX_STRING {
            return err("templated text too long");
        }
    }
    Ok(Value::String(out))
}

/// Render every string inside a JSON value (object keys are left as they are).
pub fn render_value(v: &Value, scope: &Scope) -> R<Value> {
    match v {
        Value::String(s) => render_str(s, scope),
        Value::Array(a) => Ok(Value::Array(a.iter().map(|x| render_value(x, scope)).collect::<R<_>>()?)),
        Value::Object(o) => Ok(Value::Object(o.iter().map(|(k, x)| Ok((k.clone(), render_value(x, scope)?))).collect::<R<Map<_, _>>>()?)),
        other => Ok(other.clone()),
    }
}

fn template_names(v: &Value, out: &mut Vec<String>) -> R<()> {
    match v {
        Value::String(s) if s.contains("{{") => {
            for seg in parse_template(s)? {
                if let Seg::Expr(e) = seg {
                    e.names(out);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                template_names(x, out)?;
            }
        }
        Value::Object(o) => {
            for x in o.values() {
                template_names(x, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// ── the playbook document ───────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Playbook {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub vars: Map<String, Value>,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "do")]
    pub action: String,
    #[serde(default)]
    pub args: Option<Value>,
    #[serde(default)]
    pub when: Option<Value>,
    #[serde(default, rename = "loop")]
    pub looping: Option<Value>,
    #[serde(default)]
    pub register: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub timeout_s: Option<u64>,
    #[serde(default)]
    pub ignore_errors: bool,
    #[serde(default)]
    pub tasks: Option<Vec<Task>>,
}

impl Task {
    fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.action.clone())
    }
}

pub fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn valid_ident(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !RESERVED.contains(&n)
}

pub fn note_path(name: &str) -> String {
    format!("{PLAYBOOK_DIR}{name}.json")
}

/// Canonical text of a JSON document: keys sorted, no whitespace.
fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(o) => {
            let mut ks: Vec<&String> = o.keys().collect();
            ks.sort();
            out.push('{');
            for (i, k) in ks.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                canonical(&o[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// SHA-256 (hex) of the playbook's canonical JSON.
pub fn playbook_hash(raw_json: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(raw_json).map_err(|e| format!("not valid JSON: {e}"))?;
    let mut c = String::new();
    canonical(&v, &mut c);
    Ok(Sha256::digest(c.as_bytes()).iter().map(|b| format!("{b:02x}")).collect())
}

// ── check (validate + plan) ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct PlanStep {
    pub path: String,
    pub name: String,
    pub action: String,
    pub when: Option<String>,
    pub looping: bool,
    pub register: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub ok: bool,
    pub problems: Vec<String>,
    pub plan: Vec<PlanStep>,
}

fn when_src(w: &Value) -> Result<Option<String>, String> {
    match w {
        Value::Bool(b) => Ok(Some(b.to_string())),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err("`when` must be a boolean or an expression string".into()),
    }
}

struct Checker<'a> {
    reg: &'a Registry,
    problems: Vec<String>,
    plan: Vec<PlanStep>,
}

impl Checker<'_> {
    fn need(&mut self, at: &str, names: Vec<String>, declared: &HashSet<String>) {
        for n in names {
            if !declared.contains(&n) {
                self.problems.push(format!("{at}: undeclared variable `{n}` (declare it in vars, set it, or register it in an earlier task)"));
            }
        }
    }

    fn expr_names(&mut self, at: &str, src: &str, declared: &HashSet<String>) {
        match parse_expr(src) {
            Ok(e) => {
                let mut n = Vec::new();
                e.names(&mut n);
                self.need(at, n, declared);
            }
            Err(e) => self.problems.push(format!("{at}: bad expression `{src}`: {e}")),
        }
    }

    fn tmpl_names(&mut self, at: &str, v: &Value, declared: &HashSet<String>) {
        let mut n = Vec::new();
        match template_names(v, &mut n) {
            Ok(()) => self.need(at, n, declared),
            Err(e) => self.problems.push(format!("{at}: bad template: {e}")),
        }
    }

    fn tasks(&mut self, tasks: &[Task], prefix: &str, declared: &mut HashSet<String>) {
        for (i, t) in tasks.iter().enumerate() {
            let path = format!("{prefix}{}", i + 1);
            let at = format!("task {path} ({})", t.label());
            let when = match &t.when {
                Some(w) => match when_src(w) {
                    Ok(s) => s,
                    Err(e) => {
                        self.problems.push(format!("{at}: {e}"));
                        None
                    }
                },
                None => None,
            };
            self.plan.push(PlanStep { path: path.clone(), name: t.label(), action: t.action.clone(), when: when.clone(), looping: t.looping.is_some(), register: t.register.clone(), tags: t.tags.clone() });
            let mut inner = declared.clone();
            if t.looping.is_some() {
                inner.insert("item".into());
                inner.insert("index".into());
            }
            if let Some(w) = &when {
                self.expr_names(&at, w, &inner);
            }
            if let Some(l) = &t.looping {
                match l {
                    Value::Array(_) => self.tmpl_names(&at, l, declared),
                    Value::String(s) if s.contains("{{") => self.tmpl_names(&at, l, declared),
                    Value::String(s) => self.expr_names(&at, s, declared),
                    _ => self.problems.push(format!("{at}: `loop` must be a list or an expression string")),
                }
            }
            if let Some(ts) = t.timeout_s {
                if ts == 0 || ts > MAX_TIMEOUT_S {
                    self.problems.push(format!("{at}: timeout_s must be 1..={MAX_TIMEOUT_S}"));
                }
            }
            let args_obj = match &t.args {
                None | Some(Value::Null) => Map::new(),
                Some(Value::Object(o)) => o.clone(),
                Some(_) => {
                    self.problems.push(format!("{at}: args must be an object"));
                    Map::new()
                }
            };
            match t.action.as_str() {
                "block" => {
                    if t.looping.is_some() || t.register.is_some() {
                        self.problems.push(format!("{at}: a block cannot have loop or register"));
                    }
                    match &t.tasks {
                        Some(sub) if !sub.is_empty() => {
                            let mut d = declared.clone();
                            self.tasks(sub, &format!("{path}."), &mut d);
                            // Names created inside a block stay visible after it.
                            declared.extend(d);
                        }
                        _ => self.problems.push(format!("{at}: a block needs a non-empty `tasks` list")),
                    }
                }
                other => {
                    if t.tasks.is_some() {
                        self.problems.push(format!("{at}: only `block` tasks have `tasks`"));
                    }
                    match other {
                        "set" => {
                            if args_obj.is_empty() {
                                self.problems.push(format!("{at}: set needs args: {{name: value}}"));
                            }
                            for (k, v) in &args_obj {
                                if !valid_ident(k) {
                                    self.problems.push(format!("{at}: `{k}` is not a usable variable name"));
                                }
                                self.tmpl_names(&at, v, &inner);
                            }
                            declared.extend(args_obj.keys().cloned());
                        }
                        "fail" | "ask" => {
                            if other == "ask" && !args_obj.contains_key("prompt") {
                                self.problems.push(format!("{at}: ask needs args.prompt"));
                            }
                            self.tmpl_names(&at, &Value::Object(args_obj.clone()), &inner);
                        }
                        "assert" => {
                            match args_obj.get("that") {
                                Some(Value::String(s)) => self.expr_names(&at, s, &inner),
                                Some(Value::Array(a)) if !a.is_empty() => {
                                    for x in a {
                                        match x.as_str() {
                                            Some(s) => self.expr_names(&at, s, &inner),
                                            None => self.problems.push(format!("{at}: assert.that entries must be expression strings")),
                                        }
                                    }
                                }
                                _ => self.problems.push(format!("{at}: assert needs args.that (an expression or a list of them)")),
                            }
                            if let Some(m) = args_obj.get("msg") {
                                self.tmpl_names(&at, m, &inner);
                            }
                        }
                        tool => {
                            if FORBIDDEN_TOOLS.contains(&tool) {
                                self.problems.push(format!("{at}: `{tool}` cannot be used inside a playbook"));
                            } else if let Some(tl) = self.reg.get(tool) {
                                if let Some(req) = tl.parameters()["required"].as_array() {
                                    for r in req.iter().filter_map(|r| r.as_str()) {
                                        if !args_obj.contains_key(r) {
                                            self.problems.push(format!("{at}: tool `{tool}` needs argument `{r}`"));
                                        }
                                    }
                                }
                            } else {
                                self.problems.push(format!("{at}: unknown action `{tool}` (not a built-in and not a tool)"));
                            }
                            self.tmpl_names(&at, &Value::Object(args_obj.clone()), &inner);
                        }
                    }
                }
            }
            if let Some(r) = &t.register {
                if !valid_ident(r) {
                    self.problems.push(format!("{at}: `{r}` is not a usable register name"));
                }
                declared.insert(r.clone());
            }
        }
    }
}

/// Validate a playbook against the tool registry and describe what a run would do.
pub fn check(pb: &Playbook, file_name: Option<&str>, reg: &Registry) -> CheckReport {
    let mut c = Checker { reg, problems: vec![], plan: vec![] };
    if !valid_name(&pb.name) {
        c.problems.push(format!("name `{}` must be 1-64 letters, digits, `-` or `_`", pb.name));
    }
    if let Some(f) = file_name {
        if f != pb.name {
            c.problems.push(format!("name `{}` does not match the note `{}`", pb.name, note_path(f)));
        }
    }
    if pb.tasks.is_empty() {
        c.problems.push("a playbook needs at least one task".into());
    }
    let mut declared = HashSet::new();
    for k in pb.vars.keys() {
        if !valid_ident(k) {
            c.problems.push(format!("vars: `{k}` is not a usable variable name"));
        }
        declared.insert(k.clone());
    }
    c.tasks(&pb.tasks, "", &mut declared);
    CheckReport { ok: c.problems.is_empty(), problems: c.problems, plan: c.plan }
}

// ── running ─────────────────────────────────────────────────────────────────

/// One agent turn for `ask` tasks.
#[async_trait]
pub trait Asker: Send + Sync {
    async fn ask(&self, prompt: &str) -> Result<String, String>;
}

#[async_trait]
impl Asker for Agent {
    async fn ask(&self, prompt: &str) -> Result<String, String> {
        let r = self.turn(None, prompt, true).await.map_err(|f| format!("no model answered: {}", f.error))?;
        match (r.reply, r.stopped) {
            (Some(t), _) => Ok(t),
            (None, s) => Err(s.unwrap_or_else(|| "the agent gave no answer".into())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Manual,
    Schedule,
}

impl Trigger {
    pub fn as_str(&self) -> &'static str {
        match self {
            Trigger::Manual => "manual",
            Trigger::Schedule => "schedule",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub path: String,
    pub name: String,
    pub action: String,
    /// "ok", "skipped" or "failed".
    pub status: String,
    /// A failure that `ignore_errors` let the run continue past.
    pub ignored: bool,
    pub result: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub playbook: String,
    pub trigger: Trigger,
    pub ok: bool,
    /// "ok" or "failed".
    pub status: String,
    pub error: Option<String>,
    pub log: Vec<LogEntry>,
}

impl RunReport {
    /// A short text recap for models and logs.
    pub fn recap(&self) -> String {
        let mut s = format!("playbook `{}` ({}): {}", self.playbook, self.trigger.as_str(), self.status.to_uppercase());
        if let Some(e) = &self.error {
            s.push_str(&format!(" - {e}"));
        }
        for l in &self.log {
            s.push_str(&format!("\n{} {} [{}] {}{}: {}", l.path, l.name, l.action, l.status, if l.ignored { " (ignored)" } else { "" }, l.result));
        }
        s
    }
}

fn short(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 200 {
        format!("{}...", one.chars().take(200).collect::<String>())
    } else {
        one
    }
}

enum Outcome {
    Skipped(String),
    Done(Result<String, String>),
}

struct Runner<'a> {
    reg: &'a Registry,
    ctx: &'a ToolCtx,
    asker: Option<&'a dyn Asker>,
    vars: Scope,
    skip_tags: HashSet<String>,
    log: Vec<LogEntry>,
}

impl Runner<'_> {
    fn push(&mut self, path: &str, t: &Task, label: &str, status: &str, ignored: bool, result: &str) {
        self.log.push(LogEntry { path: path.into(), name: label.into(), action: t.action.clone(), status: status.into(), ignored, result: short(result) });
    }

    fn fatal(&mut self, path: &str, t: &Task, label: &str, msg: String) -> String {
        self.push(path, t, label, "failed", false, &msg);
        format!("task {path} ({label}): {msg}")
    }

    fn eval_when(&self, t: &Task) -> R<bool> {
        match &t.when {
            None => Ok(true),
            Some(Value::Bool(b)) => Ok(*b),
            Some(Value::String(s)) => Ok(truthy(&eval_str(s, &self.vars)?)),
            Some(_) => err("`when` must be a boolean or an expression string"),
        }
    }

    fn items(&self, l: &Value) -> R<Vec<Value>> {
        let v = match l {
            Value::Array(_) => render_value(l, &self.vars)?,
            Value::String(s) if s.contains("{{") => render_str(s, &self.vars)?,
            Value::String(s) => eval_str(s, &self.vars)?,
            _ => return err("`loop` must be a list or an expression string"),
        };
        match v {
            Value::Array(a) if a.len() > MAX_LOOP => err(format!("loop has {} items; the limit is {MAX_LOOP}", a.len())),
            Value::Array(a) => Ok(a),
            other => err(format!("`loop` must produce a list, got a {}", kind(&other))),
        }
    }

    async fn run_tasks(&mut self, tasks: &[Task], prefix: &str) -> Result<(), String> {
        for (i, t) in tasks.iter().enumerate() {
            self.run_task(t, &format!("{prefix}{}", i + 1)).await?;
        }
        Ok(())
    }

    fn set_register(&mut self, t: &Task, v: Value) {
        if let Some(r) = &t.register {
            self.vars.insert(r.clone(), v);
        }
    }

    async fn run_task(&mut self, t: &Task, path: &str) -> Result<(), String> {
        let label = t.label();
        if t.tags.iter().any(|g| self.skip_tags.contains(g)) {
            self.push(path, t, &label, "skipped", false, "skipped by tag");
            self.set_register(t, json!({"ok": true, "skipped": true, "result": "", "error": ""}));
            return Ok(());
        }
        let items: Option<Vec<Value>> = match &t.looping {
            Some(l) => match self.items(l) {
                Ok(v) => Some(v),
                Err(e) => return Err(self.fatal(path, t, &label, format!("loop: {e}"))),
            },
            None => None,
        };
        let iterations: Vec<Option<(usize, Value)>> = match items {
            Some(v) => v.into_iter().enumerate().map(Some).collect(),
            None => vec![None],
        };
        let looped = t.looping.is_some();
        let mut results = Vec::new();
        let mut first_fail: Option<String> = None;
        for it in iterations {
            let (ipath, ilabel) = match &it {
                Some((i, _)) => (format!("{path}[{i}]"), format!("{label} [{i}]")),
                None => (path.to_string(), label.clone()),
            };
            let item_val = it.as_ref().map(|(_, v)| v.clone());
            if let Some((i, v)) = it {
                self.vars.insert("item".into(), v);
                self.vars.insert("index".into(), Value::from(i));
            }
            let outcome = self.step(t, &ipath, &ilabel).await;
            if looped {
                self.vars.remove("item");
                self.vars.remove("index");
            }
            let outcome = outcome?;
            let (ok, text, error, skipped) = match outcome {
                Outcome::Skipped(why) => {
                    self.push(&ipath, t, &ilabel, "skipped", false, &why);
                    (true, String::new(), String::new(), true)
                }
                Outcome::Done(Ok(s)) => {
                    self.push(&ipath, t, &ilabel, "ok", false, &s);
                    (true, s, String::new(), false)
                }
                Outcome::Done(Err(e)) => {
                    self.push(&ipath, t, &ilabel, "failed", t.ignore_errors, &e);
                    if !t.ignore_errors && first_fail.is_none() {
                        first_fail = Some(format!("task {ipath} ({ilabel}): {e}"));
                    }
                    (false, String::new(), e, false)
                }
            };
            results.push(json!({"ok": ok, "result": text, "error": error, "skipped": skipped, "item": item_val}));
            if first_fail.is_some() {
                break;
            }
        }
        let last = results.last().cloned().unwrap_or(json!({"ok": true, "result": "", "error": ""}));
        let all_ok = results.iter().all(|r| r["ok"] == true);
        let reg = if looped {
            json!({"ok": all_ok, "result": last["result"], "error": results.iter().find(|r| r["ok"] == false).map(|r| r["error"].clone()).unwrap_or(json!("")), "results": results})
        } else {
            json!({"ok": last["ok"], "result": last["result"], "error": last["error"], "skipped": last["skipped"]})
        };
        self.set_register(t, reg);
        match first_fail {
            Some(m) => Err(m),
            None => Ok(()),
        }
    }

    /// One execution (one loop iteration). `Err` is a fatal problem (bad expression, ...).
    async fn step(&mut self, t: &Task, path: &str, label: &str) -> Result<Outcome, String> {
        match self.eval_when(t) {
            Ok(true) => {}
            Ok(false) => return Ok(Outcome::Skipped("when is false".into())),
            Err(e) => return Err(self.fatal(path, t, label, format!("when: {e}"))),
        }
        if t.action == "block" {
            let sub = t.tasks.as_deref().unwrap_or(&[]);
            return Ok(Outcome::Done(match Box::pin(self.run_tasks(sub, &format!("{path}."))).await {
                Ok(()) => Ok(format!("{} task(s)", sub.len())),
                Err(e) => Err(format!("a task in the block failed: {e}")),
            }));
        }
        let args = match &t.args {
            None | Some(Value::Null) => json!({}),
            Some(a) => a.clone(),
        };
        // `assert.that` holds expressions, not templates; everything else is templated.
        let args = if t.action == "assert" {
            let mut a = args;
            if let Some(m) = a.get("msg").cloned() {
                match render_value(&m, &self.vars) {
                    Ok(v) => a["msg"] = v,
                    Err(e) => return Err(self.fatal(path, t, label, format!("args: {e}"))),
                }
            }
            a
        } else {
            match render_value(&args, &self.vars) {
                Ok(v) => v,
                Err(e) => return Err(self.fatal(path, t, label, format!("args: {e}"))),
            }
        };
        let timeout = Duration::from_secs(t.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S).clamp(1, MAX_TIMEOUT_S));
        match t.action.as_str() {
            "set" => {
                let Some(o) = args.as_object() else { return Err(self.fatal(path, t, label, "set needs args".into())) };
                let mut names = Vec::new();
                for (k, v) in o {
                    if !valid_ident(k) {
                        return Err(self.fatal(path, t, label, format!("`{k}` is not a usable variable name")));
                    }
                    self.vars.insert(k.clone(), v.clone());
                    names.push(k.as_str());
                }
                Ok(Outcome::Done(Ok(format!("set {}", names.join(", ")))))
            }
            "fail" => Ok(Outcome::Done(Err(args["msg"].as_str().map(str::to_string).unwrap_or_else(|| "fail".into())))),
            "assert" => {
                let list: Vec<String> = match &args["that"] {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                    _ => return Err(self.fatal(path, t, label, "assert needs args.that".into())),
                };
                for src in &list {
                    match eval_str(src, &self.vars) {
                        Ok(v) if truthy(&v) => {}
                        Ok(_) => {
                            let m = args["msg"].as_str().map(str::to_string).unwrap_or_else(|| format!("assertion failed: {src}"));
                            return Ok(Outcome::Done(Err(m)));
                        }
                        Err(e) => return Err(self.fatal(path, t, label, format!("assert `{src}`: {e}"))),
                    }
                }
                Ok(Outcome::Done(Ok(format!("{} assertion(s) held", list.len()))))
            }
            "ask" => {
                let Some(a) = self.asker else { return Ok(Outcome::Done(Err("`ask` is not available here (no agent attached)".into()))) };
                let Some(p) = args["prompt"].as_str() else { return Err(self.fatal(path, t, label, "ask needs args.prompt".into())) };
                Ok(Outcome::Done(match tokio::time::timeout(timeout, a.ask(p)).await {
                    Ok(r) => r,
                    Err(_) => Err(format!("timed out after {}s", timeout.as_secs())),
                }))
            }
            tool => {
                if FORBIDDEN_TOOLS.contains(&tool) {
                    return Err(self.fatal(path, t, label, format!("`{tool}` cannot be used inside a playbook")));
                }
                if !self.ctx.owner {
                    return Err(self.fatal(path, t, label, "playbooks run in the owner context only".into()));
                }
                let Some(tl) = self.reg.allowed(tool, true, false) else { return Err(self.fatal(path, t, label, format!("unknown action `{tool}`"))) };
                Ok(Outcome::Done(match tokio::time::timeout(timeout, tl.call(&args, self.ctx)).await {
                    Ok(r) => r,
                    Err(_) => Err(format!("timed out after {}s", timeout.as_secs())),
                }))
            }
        }
    }
}

/// Run a parsed playbook (no approval or storage here).
pub async fn run_playbook(pb: &Playbook, run_vars: &Map<String, Value>, skip_tags: &[String], trigger: Trigger, reg: &Registry, ctx: &ToolCtx, asker: Option<&dyn Asker>) -> RunReport {
    let mut vars: Scope = pb.vars.clone().into_iter().collect();
    let fail = |e: String| RunReport { playbook: pb.name.clone(), trigger, ok: false, status: "failed".into(), error: Some(e), log: vec![] };
    for (k, v) in run_vars {
        if !pb.vars.contains_key(k) {
            return fail(format!("run variable `{k}` is not declared in the playbook's vars"));
        }
        vars.insert(k.clone(), v.clone());
    }
    let mut r = Runner { reg, ctx, asker, vars, skip_tags: skip_tags.iter().cloned().collect(), log: vec![] };
    let res = r.run_tasks(&pb.tasks, "").await;
    let log = r.log;
    match res {
        Ok(()) => RunReport { playbook: pb.name.clone(), trigger, ok: true, status: "ok".into(), error: None, log },
        Err(e) => RunReport { playbook: pb.name.clone(), trigger, ok: false, status: "failed".into(), error: Some(e), log },
    }
}

// ── vault + approval ────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum PbError {
    NotFound(String),
    Invalid(Vec<String>),
    NotApproved(String),
    Other(String),
}

impl std::fmt::Display for PbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PbError::NotFound(m) | PbError::NotApproved(m) | PbError::Other(m) => write!(f, "{m}"),
            PbError::Invalid(p) => write!(f, "playbook is not valid: {}", p.join("; ")),
        }
    }
}

/// Load a playbook note: (parsed, raw text, hash).
pub async fn load(ctx: &ToolCtx, name: &str) -> Result<(Playbook, String, String), PbError> {
    if !valid_name(name) {
        return Err(PbError::Invalid(vec![format!("`{name}` is not a valid playbook name")]));
    }
    let n = note_by_path(&ctx.vault, &note_path(name)).await.map_err(PbError::Other)?.ok_or_else(|| PbError::NotFound(format!("no playbook `{name}` (expected the note {})", note_path(name))))?;
    let pb: Playbook = serde_json::from_str(&n.content).map_err(|e| PbError::Invalid(vec![format!("{}: {e}", note_path(name))]))?;
    let sha = playbook_hash(&n.content).map_err(PbError::Other)?;
    Ok((pb, n.content, sha))
}

pub async fn is_approved(ctx: &ToolCtx, name: &str, sha: &str) -> Result<bool, String> {
    Ok(ctx.store.playbook_approval(name).await?.is_some_and(|(s, _)| s == sha))
}

/// All playbooks with their state.
pub async fn list(ctx: &ToolCtx, reg: &Registry) -> Result<Vec<Value>, String> {
    let mut names: Vec<String> = ctx.vault.list_items().await.map_err(|e| e.to_string())?.into_iter().filter_map(|n| n.title.trim_start_matches('/').strip_prefix(PLAYBOOK_DIR).and_then(|r| r.strip_suffix(".json")).map(str::to_string)).collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let last = ctx.store.last_playbook_run(&name).await?.map(|(at, trig, ok)| json!({"at": at, "trigger": trig, "ok": ok}));
        match load(ctx, &name).await {
            Ok((pb, _, sha)) => {
                let rep = check(&pb, Some(&name), reg);
                out.push(json!({"name": name, "description": pb.description, "tasks": pb.tasks.len(), "valid": rep.ok, "approved": is_approved(ctx, &name, &sha).await?, "sha256": sha, "last_run": last}));
            }
            Err(e) => out.push(json!({"name": name, "valid": false, "approved": false, "error": e.to_string(), "last_run": last})),
        }
    }
    Ok(out)
}

pub async fn check_named(ctx: &ToolCtx, reg: &Registry, name: &str) -> Result<CheckReport, PbError> {
    match load(ctx, name).await {
        Ok((pb, _, _)) => Ok(check(&pb, Some(name), reg)),
        Err(PbError::Invalid(p)) if valid_name(name) => Ok(CheckReport { ok: false, problems: p, plan: vec![] }),
        Err(e) => Err(e),
    }
}

/// Approve the playbook as it is now (it must pass the check).
pub async fn approve(ctx: &ToolCtx, reg: &Registry, name: &str) -> Result<String, PbError> {
    let (pb, _, sha) = load(ctx, name).await?;
    let rep = check(&pb, Some(name), reg);
    if !rep.ok {
        return Err(PbError::Invalid(rep.problems));
    }
    ctx.store.approve_playbook(name, &sha).await.map_err(PbError::Other)?;
    Ok(sha)
}

/// Load, check, gate on approval (schedule runs), run and log. A failed task is a failed run.
pub async fn run_named(ctx: &ToolCtx, reg: &Registry, asker: Option<&dyn Asker>, name: &str, vars: &Map<String, Value>, skip_tags: &[String], trigger: Trigger) -> Result<RunReport, PbError> {
    let (pb, _, sha) = load(ctx, name).await?;
    let rep = check(&pb, Some(name), reg);
    if !rep.ok {
        return Err(PbError::Invalid(rep.problems));
    }
    if trigger == Trigger::Schedule {
        match ctx.store.playbook_approval(name).await.map_err(PbError::Other)? {
            Some((s, _)) if s == sha => {}
            Some(_) => return Err(PbError::NotApproved(format!("playbook `{name}` changed since it was approved; approve it again before it runs from a schedule"))),
            None => return Err(PbError::NotApproved(format!("playbook `{name}` is not approved; approve it before it runs from a schedule"))),
        }
    }
    let report = run_playbook(&pb, vars, skip_tags, trigger, reg, ctx, asker).await;
    ctx.store.log_playbook_run(name, trigger.as_str(), report.ok, &serde_json::to_value(&report).unwrap_or(Value::Null)).await.map_err(PbError::Other)?;
    Ok(report)
}

// ── the `playbook` tool ─────────────────────────────────────────────────────

/// Owner-only tool. Holds the agent weakly: it needs the agent's registry and its `ask` turn.
pub struct PlaybookTool {
    pub agent: Weak<Agent>,
}

#[async_trait]
impl Tool for PlaybookTool {
    fn name(&self) -> &str {
        "playbook"
    }
    fn description(&self) -> &str {
        "Playbooks: deterministic JSON task lists in Jade/Playbooks/<name>.json that run tools with no model in the loop. Actions: list, show (name), check (name: validate and plan without running), run (name, vars?, skip_tags?), approve (name: lets it run from schedules until the file changes). Write a playbook with the `write` tool."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["list","show","check","run","approve"]},"name":{"type":"string"},"vars":{"type":"object"},"skip_tags":{"type":"array","items":{"type":"string"}}},"required":["action"]})
    }
    async fn call(&self, a: &Value, ctx: &ToolCtx) -> Result<String, String> {
        let agent = self.agent.upgrade().ok_or("the agent is shutting down")?;
        let reg = &agent.tools;
        let name = || a["name"].as_str().ok_or_else(|| "missing string argument `name`".to_string());
        match a["action"].as_str().ok_or("missing string argument `action`")? {
            "list" => Ok(serde_json::to_string_pretty(&list(ctx, reg).await?).unwrap_or_default()),
            "show" => {
                let (_, raw, sha) = load(ctx, name()?).await.map_err(|e| e.to_string())?;
                Ok(format!("sha256: {sha}\napproved: {}\n{raw}", is_approved(ctx, name()?, &sha).await?))
            }
            "check" => Ok(serde_json::to_string_pretty(&check_named(ctx, reg, name()?).await.map_err(|e| e.to_string())?).unwrap_or_default()),
            "approve" => approve(ctx, reg, name()?).await.map(|sha| format!("approved `{}` at sha256 {sha}", name().unwrap_or(""))).map_err(|e| e.to_string()),
            "run" => {
                let vars = a["vars"].as_object().cloned().unwrap_or_default();
                let tags: Vec<String> = a["skip_tags"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(str::to_string)).collect();
                let rep = run_named(ctx, reg, Some(agent.as_ref() as &dyn Asker), name()?, &vars, &tags, Trigger::Manual).await.map_err(|e| e.to_string())?;
                if rep.ok {
                    Ok(rep.recap())
                } else {
                    Err(rep.recap())
                }
            }
            other => Err(format!("unknown action `{other}`")),
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use nautik9_wayfinder::WayfinderVault;
    use std::sync::Arc;

    fn sc(pairs: &[(&str, Value)]) -> Scope {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn ev(src: &str, s: &Scope) -> Value {
        eval_str(src, s).unwrap_or_else(|e| panic!("`{src}`: {e}"))
    }

    #[test]
    fn precedence_and_literals() {
        let s = Scope::new();
        assert_eq!(ev("1 + 2 == 3 and not false", &s), json!(true));
        assert_eq!(ev("true or false and false", &s), json!(true), "and binds tighter than or");
        assert_eq!(ev("(true or false) and false", &s), json!(false));
        assert_eq!(ev("not 1 == 2", &s), json!(true), "not applies to the whole comparison");
        assert_eq!(ev("2 - 3 - 4", &s), json!(-5), "left associative");
        assert_eq!(ev("1 < 2 and 'a' < 'b'", &s), json!(true));
        assert_eq!(ev("1.5 + 1.5", &s), json!(3));
        assert_eq!(ev("'a' + 'b'", &s), json!("ab"));
        assert_eq!(ev("[1, 2] + [3]", &s), json!([1, 2, 3]));
        assert_eq!(ev("2 in [1, 2]", &s), json!(true));
        assert_eq!(ev("3 not in [1, 2]", &s), json!(true));
        assert_eq!(ev("'ell' in 'hello'", &s), json!(true));
        assert_eq!(ev("'hello' contains 'ell'", &s), json!(true));
        assert_eq!(ev("'hello' startswith 'he' and 'hello' endswith 'lo'", &s), json!(true));
        assert_eq!(ev("length('héllo') == 5 and length([1,2,3]) == 3", &s), json!(true));
        assert_eq!(ev("upper(trim('  a '))", &s), json!("A"));
        assert_eq!(ev("int('42') + 1", &s), json!(43));
        assert_eq!(ev("json('{\"a\": [1,2]}').a[1]", &s), json!(2));
        assert_eq!(ev("[10, 20, 30][-1]", &s), json!(30));
        assert_eq!(ev("'it\\'s'", &s), json!("it's"));
    }

    #[test]
    fn variables_fields_and_errors() {
        let s = sc(&[("r", json!({"ok": true, "result": "done", "error": ""})), ("n", json!(3))]);
        assert_eq!(ev("r.ok and r.result == 'done' and n > 2", &s), json!(true));
        assert_eq!(ev("r['result']", &s), json!("done"));
        assert_eq!(ev("defined(nope)", &s), json!(false));
        assert_eq!(ev("defined(n)", &s), json!(true));
        assert_eq!(eval_str("nope == 1", &s), Err(ExprError::Undeclared("nope".into())), "undeclared is an error, not an empty string");
        assert_eq!(eval_str("'a' + nope", &s), Err(ExprError::Undeclared("nope".into())));
        assert!(eval_str("r.missing", &s).is_err());
        assert!(eval_str("1 < 'a'", &s).is_err());
        assert!(eval_str("n + 'a'", &s).is_err());
        assert!(eval_str("[1][5]", &s).is_err());
        assert!(eval_str("frobnicate(1)", &s).is_err());
        assert!(eval_str("1 +", &s).is_err());
        assert!(eval_str("(1", &s).is_err());
        assert!(eval_str("a b", &s).is_err());
        assert!(eval_str("1; 2", &s).is_err());
        assert!(eval_str("'open", &s).is_err());
        assert!(eval_str("", &s).is_err());
        // short-circuit: the right side of `or` is not evaluated when the left already decides
        assert_eq!(ev("true or nope", &s), json!(true).clone());
    }

    #[test]
    fn expression_limits() {
        let deep = format!("{}1{}", "(".repeat(200), ")".repeat(200));
        assert!(parse_expr(&deep).is_err());
        assert!(parse_expr(&"1+".repeat(3000)).is_err(), "too long");
    }

    #[test]
    fn regex_is_guarded() {
        let s = Scope::new();
        assert_eq!(ev("matches('abc123', '^[a-z]+[0-9]+$')", &s), json!(true));
        // classic catastrophic pattern returns at once on a linear-time engine
        let t = std::time::Instant::now();
        let bad = format!("matches('{}!', '(a+)+$')", "a".repeat(3000));
        assert_eq!(ev(&bad, &s), json!(false));
        assert!(t.elapsed() < Duration::from_secs(2));
        assert!(eval_str(&format!("matches('x', '{}')", "a".repeat(300)), &s).is_err(), "pattern too long");
        assert!(eval_str("matches('x', '(unclosed')", &s).is_err());
        assert!(eval_str("matches('x', '(?:a{1000}){1000}')", &s).is_err(), "compiled size limit");
        assert!(guarded_regex_is_match(&"a".repeat(MAX_REGEX_HAYSTACK + 1), "a").is_err());
    }

    #[test]
    fn templating() {
        let s = sc(&[("name", json!("Mack")), ("n", json!(2)), ("xs", json!([1, 2]))]);
        assert_eq!(render_str("Hi {{ name }}!", &s).unwrap(), json!("Hi Mack!"));
        assert_eq!(render_str("{{ n }}", &s).unwrap(), json!(2), "a lone expression keeps its type");
        assert_eq!(render_str("{{ xs }}", &s).unwrap(), json!([1, 2]));
        assert_eq!(render_str("n={{ n }} xs={{ xs }}", &s).unwrap(), json!("n=2 xs=[1,2]"));
        assert_eq!(render_str("{{ '}}' + name }}", &s).unwrap(), json!("}}Mack"), "braces inside quotes");
        assert_eq!(render_str("no templates", &s).unwrap(), json!("no templates"));
        assert!(render_str("{{ nope }}", &s).is_err());
        assert!(render_str("{{ name", &s).is_err());
        assert_eq!(render_value(&json!({"a": ["{{ name }}", 1], "{{ name }}": true}), &s).unwrap(), json!({"a": ["Mack", 1], "{{ name }}": true}), "keys are not templated");
    }

    // ── runner ──

    fn ctx() -> ToolCtx {
        ToolCtx { vault: Arc::new(WayfinderVault::in_memory().unwrap()), store: Store::in_memory().unwrap(), http: reqwest::Client::new(), secrets: Arc::new(Default::default()), search: Arc::new(vec![]), owner: true, mcp_local: Arc::new(Default::default()) }
    }

    fn pb(v: Value) -> Playbook {
        serde_json::from_value(v).unwrap()
    }

    async fn run(v: Value) -> RunReport {
        let c = ctx();
        let reg = Registry::standard();
        let p = pb(v);
        let rep = check(&p, None, &reg);
        assert!(rep.ok, "check failed: {:?}", rep.problems);
        run_playbook(&p, &Map::new(), &[], Trigger::Manual, &reg, &c, None).await
    }

    fn statuses(r: &RunReport) -> Vec<String> {
        r.log.iter().map(|l| format!("{}:{}", l.path, l.status)).collect()
    }

    #[tokio::test]
    async fn set_when_register_and_assert() {
        let r = run(json!({"name":"t","vars":{"n":5},"tasks":[
            {"do":"set","args":{"greeting":"n is {{ n }}","big":"{{ n > 3 }}"},"register":"s"},
            {"do":"assert","args":{"that":["s.ok","greeting == 'n is 5'","big"]}},
            {"name":"skipme","do":"fail","args":{"msg":"never"},"when":"n < 3"},
            {"do":"set","args":{"after":"{{ s.result }}"},"when":"defined(s)"}
        ]})).await;
        assert!(r.ok, "{}", r.recap());
        assert_eq!(statuses(&r), ["1:ok", "2:ok", "3:skipped", "4:ok"]);
    }

    #[tokio::test]
    async fn failure_stops_and_is_reported() {
        let r = run(json!({"name":"t","tasks":[
            {"do":"fail","args":{"msg":"boom"}},
            {"do":"set","args":{"x":1}}
        ]})).await;
        assert!(!r.ok);
        assert_eq!(r.status, "failed");
        assert_eq!(r.log.len(), 1, "later tasks do not run");
        assert!(r.error.as_deref().unwrap().contains("boom"));
        // a false assertion fails too
        let r = run(json!({"name":"t","tasks":[{"do":"assert","args":{"that":"1 == 2","msg":"nope"}}]})).await;
        assert!(!r.ok && r.error.unwrap().contains("nope"));
    }

    #[tokio::test]
    async fn ignore_errors_continues_but_records_failure() {
        let r = run(json!({"name":"t","tasks":[
            {"do":"fail","args":{"msg":"soft"},"ignore_errors":true,"register":"f"},
            {"do":"assert","args":{"that":["not f.ok","f.error == 'soft'"]}}
        ]})).await;
        assert!(r.ok, "{}", r.recap());
        assert_eq!(r.log[0].status, "failed");
        assert!(r.log[0].ignored);
        // an undeclared name at run time is NOT ignorable
        let c = ctx();
        let reg = Registry::standard();
        let p = pb(json!({"name":"t","tasks":[{"do":"set","args":{"x":"{{ ghost }}"},"ignore_errors":true}]}));
        assert!(!check(&p, None, &reg).ok, "check catches it statically");
        let r = run_playbook(&p, &Map::new(), &[], Trigger::Manual, &reg, &c, None).await;
        assert!(!r.ok && r.error.unwrap().contains("undeclared variable `ghost`"));
    }

    #[tokio::test]
    async fn loops_register_results_and_item() {
        let r = run(json!({"name":"t","vars":{"xs":["a","b","c"]},"tasks":[
            {"do":"set","args":{"v":"{{ item + str(index) }}"},"loop":"xs","register":"l"},
            {"do":"assert","args":{"that":["l.ok","length(l.results) == 3","v == 'c2'"]}},
            {"do":"fail","args":{"msg":"bad {{ item }}"},"loop":["x","y"],"when":"item == 'y'","register":"w"},
        ]})).await;
        assert!(!r.ok);
        let s = statuses(&r);
        assert_eq!(&s[..2], ["1[0]:ok", "1[1]:ok"]);
        assert!(s.contains(&"3[0]:skipped".to_string()) && s.contains(&"3[1]:failed".to_string()));
    }

    #[tokio::test]
    async fn loop_over_registered_value_and_cap() {
        let r = run(json!({"name":"t","tasks":[
            {"do":"set","args":{"xs":[1,2]}},
            {"do":"set","args":{"y":"{{ item }}"},"loop":"{{ xs }}"},
        ]})).await;
        assert!(r.ok, "{}", r.recap());
        let ok200: Vec<i32> = (0..200).collect();
        let r = run(json!({"name":"t","tasks":[{"do":"set","args":{"y":"{{ item }}"},"loop":ok200}]})).await;
        assert!(r.ok, "exactly 200 is allowed: {}", r.recap());
        let too_many: Vec<i32> = (0..201).collect();
        let r = run(json!({"name":"t","tasks":[{"do":"set","args":{"y":"{{ item }}"},"loop":too_many}]})).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("limit is 200"));
        assert!(r.log.iter().all(|l| l.status != "ok"), "nothing ran");
    }

    #[tokio::test]
    async fn skip_tags_and_blocks() {
        let c = ctx();
        let reg = Registry::standard();
        let p = pb(json!({"name":"t","tasks":[
            {"do":"set","args":{"a":1},"tags":["slow"],"register":"r1"},
            {"name":"grp","do":"block","tags":["grp"],"tasks":[
                {"do":"set","args":{"b":2}},
                {"do":"set","args":{"c":"{{ b }}"},"tags":["slow"]}
            ]},
            {"do":"block","ignore_errors":true,"tasks":[{"do":"fail","args":{"msg":"inner"}},{"do":"set","args":{"never":1}}]},
            {"do":"assert","args":{"that":["r1.skipped","not defined(never)"]}}
        ]}));
        assert!(check(&p, None, &reg).ok);
        let r = run_playbook(&p, &Map::new(), &["slow".into()], Trigger::Manual, &reg, &c, None).await;
        assert!(r.ok, "{}", r.recap());
        let s = statuses(&r);
        assert!(s.contains(&"1:skipped".to_string()));
        assert!(s.contains(&"2.1:ok".to_string()) && s.contains(&"2.2:skipped".to_string()));
        assert!(!s.contains(&"3.2:ok".to_string()), "block stops at its first failure");
        let r = run_playbook(&p, &Map::new(), &["grp".into()], Trigger::Manual, &reg, &c, None).await;
        assert!(r.log.iter().all(|l| !l.path.starts_with("2.")), "a skipped block hides its tasks");
        // a failing block without ignore_errors fails the run
        let r = run(json!({"name":"t","tasks":[{"do":"block","tasks":[{"do":"fail","args":{"msg":"x"}}]},{"do":"set","args":{"z":1}}]})).await;
        assert!(!r.ok);
        assert_eq!(r.log.last().unwrap().path, "1");
    }

    #[tokio::test]
    async fn run_vars_must_be_declared() {
        let c = ctx();
        let reg = Registry::standard();
        let p = pb(json!({"name":"t","vars":{"who":"x"},"tasks":[{"do":"assert","args":{"that":"who == 'y'"}}]}));
        let mut v = Map::new();
        v.insert("who".into(), json!("y"));
        assert!(run_playbook(&p, &v, &[], Trigger::Manual, &reg, &c, None).await.ok);
        v.insert("other".into(), json!(1));
        let r = run_playbook(&p, &v, &[], Trigger::Manual, &reg, &c, None).await;
        assert!(!r.ok && r.error.unwrap().contains("not declared"));
    }

    #[test]
    fn check_finds_problems() {
        let reg = Registry::standard();
        let bad = |v: Value| check(&pb(v), None, &reg).problems.join(" | ");
        assert!(bad(json!({"name":"t","tasks":[{"do":"set","args":{"a":"{{ b }}"}}]})).contains("undeclared variable `b`"));
        assert!(bad(json!({"name":"t","tasks":[{"do":"nosuchtool"}]})).contains("unknown action"));
        assert!(bad(json!({"name":"t","tasks":[{"do":"playbook"}]})).contains("cannot be used"));
        assert!(bad(json!({"name":"t","tasks":[{"do":"delegate","args":{"task":"x"}}]})).contains("cannot be used"));
        assert!(bad(json!({"name":"t","tasks":[{"do":"read"}]})).contains("needs argument `path`"));
        assert!(bad(json!({"name":"t","tasks":[{"do":"set","args":{"a":1},"when":"1 +"}]})).contains("bad expression"));
        assert!(bad(json!({"name":"t","tasks":[{"do":"set","args":{"a":1},"register":"item"}]})).contains("register name"));
        assert!(bad(json!({"name":"t","tasks":[{"do":"block"}]})).contains("non-empty"));
        assert!(bad(json!({"name":"t","tasks":[{"do":"set","args":{"a":"{{ item }}"}}]})).contains("`item`"), "item only exists inside a loop");
        assert!(bad(json!({"name":"bad name","tasks":[{"do":"set","args":{"a":1}}]})).contains("name"));
        assert!(bad(json!({"name":"t","tasks":[]})).contains("at least one"));
        assert!(serde_json::from_value::<Playbook>(json!({"name":"t","tasks":[{"do":"set","wehn":"x"}]})).is_err(), "typos in field names are rejected");
        let ok = check(&pb(json!({"name":"t","vars":{"a":1},"tasks":[{"do":"set","args":{"b":"{{ a }}"},"register":"r"},{"do":"assert","args":{"that":"r.ok and b == 1"}}]})), None, &reg);
        assert!(ok.ok, "{:?}", ok.problems);
        assert_eq!(ok.plan.len(), 2);
        assert_eq!(check(&pb(json!({"name":"x","tasks":[{"do":"set","args":{"a":1}}]})), Some("other"), &reg).problems.len(), 1, "name must match the note");
    }

    #[test]
    fn hash_ignores_formatting_not_content() {
        let a = playbook_hash("{\"name\":\"t\",\"tasks\":[{\"do\":\"set\",\"args\":{\"a\":1,\"b\":2}}]}").unwrap();
        let b = playbook_hash("{ \"tasks\": [ {\"args\": {\"b\": 2, \"a\": 1}, \"do\": \"set\"} ],\n \"name\": \"t\" }").unwrap();
        let c = playbook_hash("{\"name\":\"t\",\"tasks\":[{\"do\":\"set\",\"args\":{\"a\":1,\"b\":3}}]}").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
        assert!(playbook_hash("nope").is_err());
    }

    async fn put(c: &ToolCtx, name: &str, v: &Value) {
        crate::tools::write_note(&c.vault, &note_path(name), &serde_json::to_string_pretty(v).unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn approval_gates_schedules_and_edits_void_it() {
        let c = ctx();
        let reg = Registry::standard();
        let mut doc = json!({"name":"nightly","tasks":[{"do":"set","args":{"a":1}}]});
        put(&c, "nightly", &doc).await;
        let none = Map::new();
        // schedule without approval: refused; manual: fine and logged as manual
        assert!(matches!(run_named(&c, &reg, None, "nightly", &none, &[], Trigger::Schedule).await, Err(PbError::NotApproved(_))));
        let m = run_named(&c, &reg, None, "nightly", &none, &[], Trigger::Manual).await.unwrap();
        assert!(m.ok && m.trigger == Trigger::Manual);
        assert_eq!(c.store.last_playbook_run("nightly").await.unwrap().unwrap().1, "manual");
        // approve, then a schedule run works
        let sha = approve(&c, &reg, "nightly").await.unwrap();
        assert!(is_approved(&c, "nightly", &sha).await.unwrap());
        let s = run_named(&c, &reg, None, "nightly", &none, &[], Trigger::Schedule).await.unwrap();
        assert!(s.ok);
        assert_eq!(c.store.last_playbook_run("nightly").await.unwrap().unwrap().1, "schedule");
        // reformatting only: still approved
        crate::tools::write_note(&c.vault, &note_path("nightly"), &serde_json::to_string(&doc).unwrap()).await.unwrap();
        assert!(run_named(&c, &reg, None, "nightly", &none, &[], Trigger::Schedule).await.is_ok());
        // content change: approval is void
        doc["tasks"][0]["args"]["a"] = json!(2);
        put(&c, "nightly", &doc).await;
        match run_named(&c, &reg, None, "nightly", &none, &[], Trigger::Schedule).await {
            Err(PbError::NotApproved(m)) => assert!(m.contains("changed")),
            other => panic!("expected NotApproved, got {other:?}"),
        }
        assert!(run_named(&c, &reg, None, "nightly", &none, &[], Trigger::Manual).await.unwrap().ok, "owner can still run it by hand");
        approve(&c, &reg, "nightly").await.unwrap();
        assert!(run_named(&c, &reg, None, "nightly", &none, &[], Trigger::Schedule).await.is_ok());
        // an invalid playbook cannot be approved
        put(&c, "broken", &json!({"name":"broken","tasks":[{"do":"nosuchtool"}]})).await;
        assert!(matches!(approve(&c, &reg, "broken").await, Err(PbError::Invalid(_))));
        assert!(matches!(run_named(&c, &reg, None, "missing", &none, &[], Trigger::Manual).await, Err(PbError::NotFound(_))));
        let l = list(&c, &reg).await.unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l.iter().find(|e| e["name"] == "nightly").unwrap()["approved"], true);
        assert_eq!(l.iter().find(|e| e["name"] == "broken").unwrap()["valid"], false);
    }

    struct Echo;
    #[async_trait]
    impl Asker for Echo {
        async fn ask(&self, p: &str) -> Result<String, String> {
            if p.contains("explode") {
                Err("model down".into())
            } else {
                Ok(format!("answer to: {p}"))
            }
        }
    }

    #[tokio::test]
    async fn ask_uses_the_agent_turn() {
        let c = ctx();
        let reg = Registry::standard();
        let p = pb(json!({"name":"t","vars":{"q":"hi"},"tasks":[
            {"do":"ask","args":{"prompt":"say {{ q }}"},"register":"a"},
            {"do":"assert","args":{"that":"a.result == 'answer to: say hi'"}},
            {"do":"ask","args":{"prompt":"explode"}}
        ]}));
        let r = run_playbook(&p, &Map::new(), &[], Trigger::Manual, &reg, &c, Some(&Echo)).await;
        assert!(!r.ok && r.error.unwrap().contains("model down"));
        assert_eq!(r.log[1].status, "ok");
        let r = run_playbook(&p, &Map::new(), &[], Trigger::Manual, &reg, &c, None).await;
        assert!(!r.ok && r.error.unwrap().contains("not available"));
    }

    #[tokio::test]
    async fn end_to_end_with_real_tools_on_an_in_memory_vault() {
        let c = ctx();
        let reg = Registry::standard();
        put(&c, "e2e", &json!({"name":"e2e","description":"write, read back, assert","vars":{"folder":"Scratch","words":["alpha","beta","gamma"]},"tasks":[
            {"name":"write notes","do":"write","args":{"path":"{{ folder }}/{{ item }}.md","content":"hello {{ item }} #{{ index }}"},"loop":"words","register":"w"},
            {"do":"assert","args":{"that":["w.ok","length(w.results) == 3"]}},
            {"name":"read one","do":"read","args":{"path":"{{ folder }}/beta.md"},"register":"r"},
            {"do":"assert","args":{"that":["r.result == 'hello beta #1'","'beta' in r.result"],"msg":"read-back differs"}},
            {"name":"edit","do":"edit","args":{"path":"Scratch/beta.md","old":"hello","new":"bye"}},
            {"do":"read","args":{"path":"Scratch/beta.md"},"register":"r2"},
            {"do":"assert","args":{"that":"r2.result startswith 'bye'"}},
            {"name":"missing read is a soft failure","do":"read","args":{"path":"Scratch/none.md"},"ignore_errors":true,"register":"m"},
            {"do":"assert","args":{"that":["not m.ok","'no note' in m.error"]}},
            {"name":"list","do":"list","args":{"prefix":"Scratch/"},"register":"l"},
            {"do":"assert","args":{"that":"length(l.result) > 0 and 'gamma' in l.result"}}
        ]})).await;
        let rep = run_named(&c, &reg, None, "e2e", &Map::new(), &[], Trigger::Manual).await.unwrap();
        assert!(rep.ok, "{}", rep.recap());
        assert_eq!(rep.log.iter().filter(|l| l.status == "failed").count(), 1, "only the ignored failure");
        let n = note_by_path(&c.vault, "Scratch/gamma.md").await.unwrap().unwrap();
        assert_eq!(n.content, "hello gamma #2");
        // a wrong assertion over real data makes the whole run FAILED
        put(&c, "e2e-bad", &json!({"name":"e2e-bad","tasks":[
            {"do":"write","args":{"path":"X/a.md","content":"one"}},
            {"do":"read","args":{"path":"X/a.md"},"register":"r"},
            {"do":"assert","args":{"that":"r.result == 'two'"}}
        ]})).await;
        let rep = run_named(&c, &reg, None, "e2e-bad", &Map::new(), &[], Trigger::Manual).await.unwrap();
        assert!(!rep.ok && rep.status == "failed");
        assert_eq!(rep.log.last().unwrap().status, "failed");
    }

    #[tokio::test]
    async fn playbooks_run_in_the_owner_context_only() {
        let c = ctx();
        let reg = Registry::standard();
        let p = pb(json!({"name":"t","tasks":[{"do":"write","args":{"path":"a.md","content":"x"}}]}));
        let mut other = c.clone();
        other.owner = false;
        let r = run_playbook(&p, &Map::new(), &[], Trigger::Manual, &reg, &other, None).await;
        assert!(!r.ok && r.error.unwrap().contains("owner"));
    }
}
