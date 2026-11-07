//! Knowledge graph with a small Datalog reasoner (the intent of Jade Vault 2.25.0's Knowledge screen).
//!
//! Facts are `pred(a, b)` in `Jade/Knowledge/facts.jsonl` (one JSON object per line:
//! `{"fact": "works_at(alice, acme)", "status": "confirmed|proposed|rejected", "by": "user|agent"}`,
//! a leading `~` makes a negative fact). Rules and constraints are plain text in
//! `Jade/Knowledge/rules.dl`:
//!
//! ```text
//! above(X, Y) :- on(X, Y).            % rule (variables start with an upper-case letter or _)
//! above(X, Z) :- above(X, Y), above(Y, Z).
//! functional works_at.                 % constraint: the first argument has one second argument
//! exclusive parent_of sibling_of.      % constraint: no pair may be in both
//! ```
//!
//! Every answer is one of three, and "unknown" is never turned into "false":
//! - **proven**: derivable, with the facts and rules it rests on (and a flag when a proposed,
//!   unconfirmed fact is among them);
//! - **contradicted**: its negation is a fact, or adding it would break a constraint;
//! - **unknown**: the graph does not say.
//!
//! The model never confirms facts: facts it adds are `proposed` until the owner confirms them, and a
//! `rejected` fact cannot be added again. Evaluation is deterministic Rust (semi-naive, indexed), no
//! model call. Derivation stops with an error past [`MAX_FACTS`] facts (a guard for huge joins; plain
//! Datalog cannot invent new names, so a rule set cannot run away by itself).

use nautik9_wayfinder::WayfinderVault;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub const FACTS_PATH: &str = "Jade/Knowledge/facts.jsonl";
pub const RULES_PATH: &str = "Jade/Knowledge/rules.dl";
pub const MAX_FACTS: usize = 500_000;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Atom {
    pub pred: String,
    pub args: Vec<String>,
}

impl std::fmt::Display for Atom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let q = |a: &String| if !ident_ok(a) || a.starts_with(|c: char| c.is_uppercase() || c == '_') { format!("\"{a}\"") } else { a.clone() };
        write!(f, "{}({})", self.pred, self.args.iter().map(q).collect::<Vec<_>>().join(", "))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Var(String),
    Const(String),
}

#[derive(Clone, Debug)]
pub struct Pat {
    pub pred: String,
    pub args: Vec<Term>,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub head: Pat,
    pub body: Vec<Pat>,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub rules: Vec<Rule>,
    pub functional: Vec<String>,
    pub exclusive: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Confirmed,
    Proposed,
    Rejected,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Confirmed => "confirmed",
            Status::Proposed => "proposed",
            Status::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Fact {
    pub atom: Atom,
    pub neg: bool,
    pub status: Status,
    pub by: String,
}

// ---- parsing ----

fn split_args(s: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut q) = (Vec::new(), String::new(), false);
    for ch in s.chars() {
        match ch {
            '"' => {
                q = !q;
                cur.push(ch)
            }
            ',' if !q => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    if q {
        return Err("unclosed quote".into());
    }
    let last = cur.trim().to_string();
    if !(last.is_empty() && out.is_empty()) {
        out.push(last);
    }
    if out.iter().any(|a| a.is_empty()) {
        return Err("empty argument".into());
    }
    Ok(out)
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == ':' || c == '/')
}

fn parse_pat(s: &str) -> Result<Pat, String> {
    let s = s.trim();
    let open = s.find('(').ok_or_else(|| format!("`{s}`: expected name(arg, ...)"))?;
    if !s.ends_with(')') {
        return Err(format!("`{s}`: missing `)`"));
    }
    let pred = s[..open].trim().to_string();
    if !ident_ok(&pred) {
        return Err(format!("bad predicate name `{pred}`"));
    }
    let raw = split_args(&s[open + 1..s.len() - 1]).map_err(|e| format!("`{s}`: {e}"))?;
    if raw.is_empty() {
        return Err(format!("`{s}`: no arguments"));
    }
    let args = raw
        .into_iter()
        .map(|a| {
            if a.len() >= 2 && a.starts_with('"') && a.ends_with('"') {
                Ok(Term::Const(a[1..a.len() - 1].to_string()))
            } else if !ident_ok(&a) {
                Err(format!("bad argument `{a}` (quote text with spaces)"))
            } else if a.starts_with(|c: char| c.is_uppercase() || c == '_') {
                Ok(Term::Var(a))
            } else {
                Ok(Term::Const(a))
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Pat { pred, args })
}

pub fn parse_atom(s: &str) -> Result<Atom, String> {
    let p = parse_pat(s)?;
    let args = p
        .args
        .into_iter()
        .map(|t| match t {
            Term::Const(c) => Ok(c),
            Term::Var(v) => Err(format!("`{v}` is a variable; a fact or claim needs lower-case names (quote names that start with a capital)")),
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Atom { pred: p.pred, args })
}

fn split_body(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut q) = (Vec::new(), String::new(), 0i32, false);
    for ch in s.chars() {
        match ch {
            '"' => {
                q = !q;
                cur.push(ch)
            }
            '(' if !q => {
                depth += 1;
                cur.push(ch)
            }
            ')' if !q => {
                depth -= 1;
                cur.push(ch)
            }
            ',' if !q && depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Parse `rules.dl`. The error names the line.
pub fn parse_rules(text: &str) -> Result<Rules, String> {
    let mut r = Rules::default();
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.split(|c| c == '%' || c == '#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let line = line.strip_suffix('.').unwrap_or(line).trim();
        let e = |m: String| format!("rules.dl line {n}: {m}");
        if let Some(rest) = line.strip_prefix("functional ") {
            let p = rest.trim();
            if !ident_ok(p) {
                return Err(e(format!("bad predicate `{p}`")));
            }
            r.functional.push(p.to_string());
        } else if let Some(rest) = line.strip_prefix("exclusive ") {
            let v: Vec<&str> = rest.split_whitespace().collect();
            if v.len() != 2 || !v.iter().all(|x| ident_ok(x)) {
                return Err(e("`exclusive` takes two predicate names".into()));
            }
            r.exclusive.push((v[0].to_string(), v[1].to_string()));
        } else if let Some((h, b)) = line.split_once(":-") {
            let head = parse_pat(h).map_err(&e)?;
            let body: Vec<Pat> = split_body(b).iter().map(|x| parse_pat(x)).collect::<Result<_, _>>().map_err(&e)?;
            if body.is_empty() {
                return Err(e("a rule needs a body after `:-`".into()));
            }
            let bound: HashSet<&String> = body.iter().flat_map(|p| p.args.iter()).filter_map(|t| if let Term::Var(v) = t { Some(v) } else { None }).collect();
            for t in &head.args {
                if let Term::Var(v) = t {
                    if !bound.contains(v) {
                        return Err(e(format!("variable {v} in the head does not appear in the body")));
                    }
                }
            }
            r.rules.push(Rule { head, body, text: line.to_string() });
        } else {
            return Err(e(format!("`{line}` is not a rule (`head :- body.`), `functional p.` or `exclusive p q.`")));
        }
    }
    Ok(r)
}

pub fn parse_fact_line(line: &str) -> Result<Fact, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("bad facts line: {e}"))?;
    let raw = v["fact"].as_str().ok_or("facts line without `fact`")?.trim();
    let (neg, body) = match raw.strip_prefix('~') {
        Some(b) => (true, b),
        None => (false, raw),
    };
    let status = match v["status"].as_str().unwrap_or("proposed") {
        "confirmed" => Status::Confirmed,
        "rejected" => Status::Rejected,
        _ => Status::Proposed,
    };
    Ok(Fact { atom: parse_atom(body)?, neg, status, by: v["by"].as_str().unwrap_or("user").to_string() })
}

pub fn fact_line(f: &Fact) -> String {
    json!({"fact": format!("{}{}", if f.neg { "~" } else { "" }, f.atom), "status": f.status.as_str(), "by": f.by}).to_string()
}

// ---- the store ----

pub struct Graph {
    pub facts: Vec<Fact>,
    pub rules: Rules,
}

impl Graph {
    pub fn new(facts: Vec<Fact>, rules: Rules) -> Self {
        Graph { facts, rules }
    }

    pub async fn load(vault: &WayfinderVault) -> Result<Self, String> {
        let mut facts = Vec::new();
        if let Some(n) = crate::tools::note_by_path(vault, FACTS_PATH).await? {
            for (i, l) in n.content.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
                facts.push(parse_fact_line(l).map_err(|e| format!("{FACTS_PATH} line {}: {e}", i + 1))?);
            }
        }
        let rules = match crate::tools::note_by_path(vault, RULES_PATH).await? {
            Some(n) => parse_rules(&n.content)?,
            None => Rules::default(),
        };
        Ok(Graph { facts, rules })
    }

    pub async fn save_facts(&self, vault: &WayfinderVault) -> Result<(), String> {
        let text: String = self.facts.iter().map(|f| fact_line(f) + "\n").collect();
        crate::tools::write_note(vault, FACTS_PATH, &text).await.map(|_| ())
    }

    fn find(&self, a: &Atom, neg: bool) -> Option<usize> {
        self.facts.iter().position(|f| f.atom == *a && f.neg == neg)
    }

    /// Add a fact. `by_agent` facts are proposed; the owner's are confirmed. A rejected fact stays out
    /// when an agent tries to add it; the owner may re-add it (that confirms it).
    pub fn add(&mut self, claim: &str, by_agent: bool) -> Result<String, String> {
        let (neg, body) = match claim.trim().strip_prefix('~') {
            Some(b) => (true, b),
            None => (false, claim.trim()),
        };
        let atom = parse_atom(body)?;
        if let Some(i) = self.find(&atom, neg) {
            let f = &mut self.facts[i];
            return match (f.status, by_agent) {
                (Status::Rejected, true) => Err(format!("{} was rejected by the owner; it cannot be added again", f.atom)),
                (_, true) => Ok(format!("already known ({})", f.status.as_str())),
                (_, false) => {
                    f.status = Status::Confirmed;
                    f.by = "user".into();
                    Ok("confirmed".into())
                }
            };
        }
        self.facts.push(Fact { atom, neg, status: if by_agent { Status::Proposed } else { Status::Confirmed }, by: if by_agent { "agent".into() } else { "user".into() } });
        Ok(if by_agent { "proposed (waits for the owner's confirmation)".into() } else { "confirmed".into() })
    }

    pub fn set_status(&mut self, claim: &str, status: Status) -> Result<usize, String> {
        let (neg, body) = match claim.trim().strip_prefix('~') {
            Some(b) => (true, b),
            None => (false, claim.trim()),
        };
        let atom = parse_atom(body)?;
        let i = self.find(&atom, neg).ok_or_else(|| format!("no such fact {atom}"))?;
        self.facts[i].status = status;
        self.facts[i].by = "user".into();
        Ok(1)
    }

    pub fn pending(&self) -> usize {
        self.facts.iter().filter(|f| f.status == Status::Proposed).count()
    }
}

// ---- the reasoner ----

struct Node {
    atom: Atom,
    /// None = a base fact; Some((rule index, premise node indices)).
    how: Option<(usize, Vec<usize>)>,
    unconfirmed: bool,
}

pub struct Model {
    nodes: Vec<Node>,
    by_atom: HashMap<Atom, usize>,
    negatives: HashMap<Atom, bool>, // atom -> unconfirmed
    rules: Rules,
}

type Binding = HashMap<String, String>;

struct Index {
    by_pred: HashMap<String, Vec<usize>>,
    by_arg: HashMap<(String, usize, String), Vec<usize>>,
}

impl Index {
    fn add(&mut self, i: usize, a: &Atom) {
        self.by_pred.entry(a.pred.clone()).or_default().push(i);
        for (p, v) in a.args.iter().enumerate().take(2) {
            self.by_arg.entry((a.pred.clone(), p, v.clone())).or_default().push(i);
        }
    }
    fn candidates(&self, pat: &Pat, b: &Binding) -> &[usize] {
        for (p, t) in pat.args.iter().enumerate().take(2) {
            let v = match t {
                Term::Const(c) => Some(c.clone()),
                Term::Var(x) => b.get(x).cloned(),
            };
            if let Some(v) = v {
                return self.by_arg.get(&(pat.pred.clone(), p, v)).map(|x| x.as_slice()).unwrap_or(&[]);
            }
        }
        self.by_pred.get(&pat.pred).map(|x| x.as_slice()).unwrap_or(&[])
    }
}

fn unify(pat: &Pat, a: &Atom, b: &Binding) -> Option<Binding> {
    if pat.pred != a.pred || pat.args.len() != a.args.len() {
        return None;
    }
    let mut nb = b.clone();
    for (t, v) in pat.args.iter().zip(&a.args) {
        match t {
            Term::Const(c) if c != v => return None,
            Term::Const(_) => {}
            Term::Var(x) => match nb.get(x) {
                Some(old) if old != v => return None,
                Some(_) => {}
                None => {
                    nb.insert(x.clone(), v.clone());
                }
            },
        }
    }
    Some(nb)
}

fn subst(pat: &Pat, b: &Binding) -> Atom {
    Atom {
        pred: pat.pred.clone(),
        args: pat.args.iter().map(|t| match t { Term::Const(c) => c.clone(), Term::Var(x) => b[x].clone() }).collect(),
    }
}

impl Model {
    /// Derive everything. Rejected facts are left out; proposed ones are used and flagged.
    pub fn derive(g: &Graph) -> Result<Model, String> {
        let mut m = Model { nodes: Vec::new(), by_atom: HashMap::new(), negatives: HashMap::new(), rules: g.rules.clone() };
        let mut idx = Index { by_pred: HashMap::new(), by_arg: HashMap::new() };
        for f in g.facts.iter().filter(|f| f.status != Status::Rejected) {
            let unconf = f.status == Status::Proposed;
            if f.neg {
                m.negatives.entry(f.atom.clone()).and_modify(|u| *u = *u && unconf).or_insert(unconf);
            } else if !m.by_atom.contains_key(&f.atom) {
                m.by_atom.insert(f.atom.clone(), m.nodes.len());
                idx.add(m.nodes.len(), &f.atom);
                m.nodes.push(Node { atom: f.atom.clone(), how: None, unconfirmed: unconf });
            }
        }
        let mut delta: Vec<usize> = (0..m.nodes.len()).collect();
        while !delta.is_empty() {
            let dset: HashSet<usize> = delta.iter().copied().collect();
            let mut fresh: Vec<(Atom, usize, Vec<usize>)> = Vec::new();
            for (ri, rule) in m.rules.rules.iter().enumerate() {
                for dpos in 0..rule.body.len() {
                    // body atom `dpos` must match a delta node; the others any node
                    let mut stack: Vec<(usize, Binding, Vec<usize>)> = vec![(0, Binding::new(), Vec::new())];
                    while let Some((pos, b, used)) = stack.pop() {
                        if pos == rule.body.len() {
                            fresh.push((subst(&rule.head, &b), ri, used));
                            continue;
                        }
                        for &ci in idx.candidates(&rule.body[pos], &b) {
                            if pos == dpos && !dset.contains(&ci) {
                                continue;
                            }
                            if let Some(nb) = unify(&rule.body[pos], &m.nodes[ci].atom, &b) {
                                let mut u = used.clone();
                                u.push(ci);
                                stack.push((pos + 1, nb, u));
                            }
                        }
                    }
                }
            }
            let mut next = Vec::new();
            for (atom, ri, prem) in fresh {
                if m.by_atom.contains_key(&atom) {
                    continue;
                }
                if m.nodes.len() >= MAX_FACTS {
                    return Err(format!("the graph would grow past {MAX_FACTS} facts; derivation stopped (check for a rule that generates new facts without end)"));
                }
                let unconf = prem.iter().any(|&p| m.nodes[p].unconfirmed);
                let i = m.nodes.len();
                m.by_atom.insert(atom.clone(), i);
                idx.add(i, &atom);
                m.nodes.push(Node { atom, how: Some((ri, prem)), unconfirmed: unconf });
                next.push(i);
            }
            delta = next;
        }
        Ok(m)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    fn proof_lines(&self, i: usize, depth: usize, out: &mut Vec<String>, seen: &mut HashSet<usize>) {
        let n = &self.nodes[i];
        let pad = "  ".repeat(depth);
        match &n.how {
            None => out.push(format!("{pad}{} (fact{})", n.atom, if n.unconfirmed { ", proposed and not confirmed" } else { "" })),
            Some((ri, prem)) => {
                out.push(format!("{pad}{} by rule: {}", n.atom, self.rules.rules[*ri].text));
                if seen.insert(i) && depth < 12 {
                    for &p in prem {
                        self.proof_lines(p, depth + 1, out, seen);
                    }
                }
            }
        }
    }

    /// Why a claim holds, or None.
    pub fn why(&self, claim: &Atom) -> Option<(Vec<String>, bool)> {
        let &i = self.by_atom.get(claim)?;
        let mut out = Vec::new();
        self.proof_lines(i, 0, &mut out, &mut HashSet::new());
        Some((out, self.nodes[i].unconfirmed))
    }

    /// proven / contradicted / unknown.
    pub fn verify(&self, claim: &Atom) -> Value {
        if let Some((proof, unconf)) = self.why(claim) {
            return json!({"verdict": "proven", "claim": claim.to_string(), "proof": proof, "rests_on_unconfirmed": unconf});
        }
        if let Some(&unconf) = self.negatives.get(claim) {
            return json!({"verdict": "contradicted", "claim": claim.to_string(), "reason": format!("the opposite is a fact: ~{claim}"), "rests_on_unconfirmed": unconf});
        }
        if self.rules.functional.contains(&claim.pred) && claim.args.len() >= 2 {
            for n in &self.nodes {
                if n.atom.pred == claim.pred && n.atom.args.len() == claim.args.len() && n.atom.args[0] == claim.args[0] && n.atom.args[1..] != claim.args[1..] {
                    return json!({"verdict": "contradicted", "claim": claim.to_string(), "reason": format!("{} is functional ({} has one value) and {} already holds", claim.pred, claim.args[0], n.atom), "rests_on_unconfirmed": n.unconfirmed});
                }
            }
        }
        for (p, q) in &self.rules.exclusive {
            let other = if *p == claim.pred { Some(q) } else if *q == claim.pred { Some(p) } else { None };
            if let Some(o) = other {
                let twin = Atom { pred: o.clone(), args: claim.args.clone() };
                if let Some(&i) = self.by_atom.get(&twin) {
                    return json!({"verdict": "contradicted", "claim": claim.to_string(), "reason": format!("{p} and {q} are exclusive and {twin} holds"), "rests_on_unconfirmed": self.nodes[i].unconfirmed});
                }
            }
        }
        json!({"verdict": "unknown", "claim": claim.to_string(), "note": "the graph does not say; missing is not false"})
    }

    /// Answers to a pattern with variables, e.g. `above(bob, X)`.
    pub fn query(&self, pattern: &str, limit: usize) -> Result<Value, String> {
        let pat = parse_pat(pattern)?;
        let mut rows = Vec::new();
        for n in self.nodes.iter().filter(|n| n.atom.pred == pat.pred) {
            if let Some(b) = unify(&pat, &n.atom, &Binding::new()) {
                let mut keys: Vec<_> = b.into_iter().collect();
                keys.sort();
                rows.push(json!({"bindings": keys.into_iter().collect::<std::collections::BTreeMap<_, _>>(), "fact": n.atom.to_string(), "rests_on_unconfirmed": n.unconfirmed}));
                if rows.len() >= limit {
                    break;
                }
            }
        }
        Ok(json!({"pattern": pattern, "count": rows.len(), "answers": rows}))
    }

    /// Every contradiction in the graph: constraint breaks and facts that are also negated.
    pub fn check(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for p in &self.rules.functional {
            let mut seen: HashMap<&String, Vec<&Node>> = HashMap::new();
            for n in self.nodes.iter().filter(|n| n.atom.pred == *p && n.atom.args.len() >= 2) {
                seen.entry(&n.atom.args[0]).or_default().push(n);
            }
            let mut keys: Vec<_> = seen.keys().copied().collect();
            keys.sort();
            for k in keys {
                let ns = &seen[k];
                if ns.len() > 1 {
                    out.push(json!({"kind": "functional", "constraint": format!("functional {p}"), "facts": ns.iter().map(|n| n.atom.to_string()).collect::<Vec<_>>(), "involves_unconfirmed": ns.iter().any(|n| n.unconfirmed)}));
                }
            }
        }
        for (p, q) in &self.rules.exclusive {
            for n in self.nodes.iter().filter(|n| n.atom.pred == *p) {
                let twin = Atom { pred: q.clone(), args: n.atom.args.clone() };
                if let Some(&j) = self.by_atom.get(&twin) {
                    out.push(json!({"kind": "exclusive", "constraint": format!("exclusive {p} {q}"), "facts": [n.atom.to_string(), twin.to_string()], "involves_unconfirmed": n.unconfirmed || self.nodes[j].unconfirmed}));
                }
            }
        }
        let mut negs: Vec<_> = self.negatives.iter().filter(|(a, _)| self.by_atom.contains_key(*a)).collect();
        negs.sort_by_key(|(a, _)| a.to_string());
        for (a, un) in negs {
            out.push(json!({"kind": "negated", "constraint": "a fact and its negation", "facts": [a.to_string(), format!("~{a}")], "involves_unconfirmed": *un || self.nodes[self.by_atom[a]].unconfirmed}));
        }
        out
    }
}

// ---- the model-facing tool and the API share this ----

/// One action. `by_agent` is true for the model (its additions are proposals; it cannot confirm or
/// reject). The owner's API passes false.
pub async fn run(vault: &WayfinderVault, action: &str, a: &Value, by_agent: bool) -> Result<Value, String> {
    let mut g = Graph::load(vault).await?;
    let text = |k: &str| a[k].as_str().ok_or_else(|| format!("missing string argument `{k}`"));
    match action {
        "add" => {
            let r = g.add(text("fact")?, by_agent)?;
            g.save_facts(vault).await?;
            Ok(json!({"ok": true, "result": r, "pending_confirmation": g.pending()}))
        }
        "confirm" | "reject" if by_agent => Err("only the owner confirms or rejects facts".into()),
        "confirm" => {
            g.set_status(text("fact")?, Status::Confirmed)?;
            g.save_facts(vault).await?;
            Ok(json!({"ok": true, "pending_confirmation": g.pending()}))
        }
        "confirm_all" if by_agent => Err("only the owner confirms facts".into()),
        "confirm_all" => {
            let mut n = 0;
            for f in g.facts.iter_mut().filter(|f| f.status == Status::Proposed) {
                f.status = Status::Confirmed;
                f.by = "user".into();
                n += 1;
            }
            g.save_facts(vault).await?;
            Ok(json!({"ok": true, "confirmed": n}))
        }
        "reject" => {
            g.set_status(text("fact")?, Status::Rejected)?;
            g.save_facts(vault).await?;
            Ok(json!({"ok": true}))
        }
        "list" => Ok(json!({
            "facts": g.facts.iter().map(|f| json!({"fact": format!("{}{}", if f.neg {"~"} else {""}, f.atom), "status": f.status.as_str(), "by": f.by})).collect::<Vec<_>>(),
            "rules": g.rules.rules.len(), "pending_confirmation": g.pending(),
        })),
        "verify" => Ok(Model::derive(&g)?.verify(&parse_atom(text("claim")?)?)),
        "why" => {
            let atom = parse_atom(text("claim")?)?;
            let m = Model::derive(&g)?;
            match m.why(&atom) {
                Some((p, u)) => Ok(json!({"holds": true, "proof": p, "rests_on_unconfirmed": u})),
                None => Ok(json!({"holds": false, "note": "not derivable from the graph (this is not a claim that it is false)"})),
            }
        }
        "query" => Ok(Model::derive(&g)?.query(text("pattern")?, a["limit"].as_u64().unwrap_or(50) as usize)?),
        "check" => {
            let m = Model::derive(&g)?;
            let c = m.check();
            Ok(json!({"contradictions": c, "count": c.len(), "facts_derived": m.len()}))
        }
        other => Err(format!("unknown action `{other}` (add, list, verify, why, query, check; owner only: confirm, confirm_all, reject)")),
    }
}

/// Replace rules.dl after parsing it; a broken file is refused and the error names the line.
pub async fn save_rules(vault: &WayfinderVault, text: &str) -> Result<(), String> {
    parse_rules(text)?;
    crate::tools::write_note(vault, RULES_PATH, text).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(facts: &[&str], rules: &str) -> Graph {
        let mut g = Graph::new(vec![], parse_rules(rules).unwrap());
        for f in facts {
            g.add(f, false).unwrap();
        }
        g
    }
    fn verdict(g: &Graph, c: &str) -> String {
        Model::derive(g).unwrap().verify(&parse_atom(c).unwrap())["verdict"].as_str().unwrap().to_string()
    }

    const RULES: &str = "above(X, Y) :- on(X, Y).\nabove(X, Z) :- above(X, Y), above(Y, Z).\nfunctional works_at.\nexclusive parent_of sibling_of.\n";

    #[test]
    fn proven_with_the_facts_and_rules_it_rests_on() {
        let g = graph(&["on(a, b)", "on(b, c)"], RULES);
        let m = Model::derive(&g).unwrap();
        let v = m.verify(&parse_atom("above(a, c)").unwrap());
        assert_eq!(v["verdict"], "proven");
        let proof = v["proof"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect::<Vec<_>>().join("\n");
        assert!(proof.contains("on(a, b) (fact)") && proof.contains("on(b, c) (fact)") && proof.contains("above(X, Z) :- above(X, Y), above(Y, Z)"), "{proof}");
        assert_eq!(v["rests_on_unconfirmed"], false);
    }

    #[test]
    fn missing_is_unknown_never_false() {
        let g = graph(&["on(a, b)"], RULES);
        assert_eq!(verdict(&g, "above(b, a)"), "unknown");
        assert_eq!(verdict(&g, "colour(sky, blue)"), "unknown");
    }

    #[test]
    fn constraints_and_negative_facts_contradict() {
        let g = graph(&["works_at(alice, acme)", "~lives_in(alice, mars)", "parent_of(p, q)"], RULES);
        let m = Model::derive(&g).unwrap();
        let v = m.verify(&parse_atom("works_at(alice, initech)").unwrap());
        assert_eq!(v["verdict"], "contradicted");
        assert!(v["reason"].as_str().unwrap().contains("works_at(alice, acme)"));
        assert_eq!(verdict(&g, "works_at(alice, acme)"), "proven");
        assert_eq!(verdict(&g, "lives_in(alice, mars)"), "contradicted");
        assert_eq!(verdict(&g, "sibling_of(p, q)"), "contradicted");
        assert_eq!(verdict(&g, "works_at(bob, acme)"), "unknown");
    }

    #[test]
    fn agent_facts_are_proposals_and_proofs_say_so() {
        let mut g = graph(&["on(a, b)"], RULES);
        assert!(g.add("on(b, c)", true).unwrap().contains("proposed"));
        assert_eq!(g.pending(), 1);
        let v = Model::derive(&g).unwrap().verify(&parse_atom("above(a, c)").unwrap());
        assert_eq!((v["verdict"].as_str(), v["rests_on_unconfirmed"].as_bool()), (Some("proven"), Some(true)));
        g.set_status("on(b, c)", Status::Confirmed).unwrap();
        let v = Model::derive(&g).unwrap().verify(&parse_atom("above(a, c)").unwrap());
        assert_eq!(v["rests_on_unconfirmed"], false);
    }

    #[test]
    fn a_rejected_fact_stays_out_and_the_agent_cannot_add_it_again() {
        let mut g = graph(&["on(a, b)"], RULES);
        g.add("on(b, c)", true).unwrap();
        g.set_status("on(b, c)", Status::Rejected).unwrap();
        assert_eq!(verdict(&g, "above(a, c)"), "unknown");
        assert!(g.add("on(b, c)", true).unwrap_err().contains("rejected"));
        assert_eq!(g.facts.iter().filter(|f| f.status == Status::Rejected).count(), 1);
    }

    #[test]
    fn check_lists_every_contradiction_with_the_facts() {
        let g = graph(&["works_at(alice, acme)", "works_at(alice, initech)", "parent_of(p, q)", "sibling_of(p, q)", "on(a, b)", "~on(a, b)"], RULES);
        let c = Model::derive(&g).unwrap().check();
        let kinds: Vec<&str> = c.iter().map(|x| x["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"functional") && kinds.contains(&"exclusive") && kinds.contains(&"negated"), "{kinds:?}");
    }

    #[test]
    fn a_broken_rules_file_is_refused_and_names_the_line() {
        let e = parse_rules("above(X, Y) :- on(X, Y).\nthis is nonsense\n").unwrap_err();
        assert!(e.contains("line 2"), "{e}");
        let e = parse_rules("p(X, Z) :- q(X, Y).").unwrap_err();
        assert!(e.contains("Z") && e.contains("line 1"), "{e}");
        assert!(parse_atom("on(A, b)").is_err(), "variables are not facts");
    }

    #[test]
    fn query_with_variables() {
        let g = graph(&["on(a, b)", "on(b, c)", "on(c, d)"], RULES);
        let v = Model::derive(&g).unwrap().query("above(b, X)", 10).unwrap();
        let xs: Vec<&str> = v["answers"].as_array().unwrap().iter().map(|a| a["bindings"]["X"].as_str().unwrap()).collect();
        assert_eq!(xs.len(), 2);
        assert!(xs.contains(&"c") && xs.contains(&"d"));
    }

    #[test]
    fn a_long_chain_closes_quickly() {
        let mut facts = Vec::new();
        for i in 0..150 {
            facts.push(Fact { atom: Atom { pred: "on".into(), args: vec![format!("n{i}"), format!("n{}", i + 1)] }, neg: false, status: Status::Confirmed, by: "user".into() });
        }
        let g = Graph::new(facts, parse_rules(RULES).unwrap());
        let t = std::time::Instant::now();
        let m = Model::derive(&g).unwrap();
        assert_eq!(m.verify(&parse_atom("above(n0, n150)").unwrap())["verdict"], "proven");
        assert!(m.len() > 11_000, "{}", m.len());
        assert!(t.elapsed().as_secs() < 20);
    }

    #[test]
    fn fact_lines_round_trip() {
        let f = parse_fact_line(r#"{"fact":"~likes(\"Mr X\", tea)","status":"proposed","by":"agent"}"#).unwrap();
        assert!(f.neg && f.status == Status::Proposed && f.atom.args[0] == "Mr X");
        assert_eq!(parse_fact_line(&fact_line(&f)).unwrap().atom, f.atom);
    }
}
