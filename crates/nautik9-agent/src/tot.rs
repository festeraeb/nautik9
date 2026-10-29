//! Tree-of-thought reasoner over the provider chain (`/api/tot`).
//!
//! Beam search, as in the Tree of Thoughts paper: from each kept node the model proposes
//! `branch` distinct next thoughts, the model scores each 0 to 10, the best `beam` thoughts
//! are kept ("promoted") and expanded, the rest are "pruned", down to `depth`. The best-scored
//! path is the winning plan, and one last model call writes the answer from that path.
//!
//! Honesty rules: every thought, score and answer is something a model returned; there is no
//! canned text. The scorer is the same model that proposed the thoughts, so the score is a
//! self-evaluation, not verification, and the result says so (`scored_by`). A reply that cannot
//! be parsed is recorded as such (score 0, with the reason), never replaced. If no endpoint
//! answers, the error and each endpoint's failure come back and nothing else.
//! `budget` caps model calls; when it runs out the search stops and answers from the best path
//! so far (`budget_exhausted: true`).

use nautik9_lemonade::{ChatFailure, LemonadeModule};
use serde::Serialize;
use serde_json::{json, Value};

use crate::agent::visible_text;

#[derive(Clone, Debug)]
pub struct TotParams {
    pub branch: usize,
    pub beam: usize,
    pub depth: usize,
    /// Max model calls, including the final answer.
    pub budget: usize,
    pub max_tokens: u32,
}

impl Default for TotParams {
    fn default() -> Self {
        TotParams { branch: 3, beam: 2, depth: 3, budget: 40, max_tokens: 700 }
    }
}

impl TotParams {
    /// From a request body; every value is clamped (branch 2-5, beam 1-3, depth 1-4, budget 4-80).
    pub fn from_json(v: &Value) -> Self {
        let d = TotParams::default();
        let n = |k: &str, dv: usize, lo: usize, hi: usize| v[k].as_u64().map(|x| (x as usize).clamp(lo, hi)).unwrap_or(dv);
        TotParams { branch: n("branch", d.branch, 2, 5), beam: n("beam", d.beam, 1, 3), depth: n("depth", d.depth, 1, 4), budget: n("budget", d.budget, 4, 80), max_tokens: d.max_tokens }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: usize,
    pub parent: Option<usize>,
    pub depth: usize,
    pub label: String,
    pub hypothesis: String,
    /// 0.0 to 1.0 (the model's 0-10 score / 10).
    pub score: f64,
    /// "promoted" (kept and expanded), "pruned", or "winning" (on the best path).
    pub status: String,
    pub verifier_checks: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TotResult {
    pub ok: bool,
    pub problem: String,
    pub nodes: Vec<Node>,
    pub winning_branch_id: Option<usize>,
    pub winning_plan: Vec<String>,
    pub answer: String,
    pub model_calls: usize,
    pub budget_exhausted: bool,
    pub scored_by: &'static str,
    pub model: String,
    pub provider: String,
}

pub enum TotError {
    /// No endpoint answered (carries each attempt).
    NoModel(ChatFailure),
    /// The model answered but not in a usable form.
    Unusable(String),
}

struct Caller<'a> {
    llm: &'a LemonadeModule,
    max_tokens: u32,
    calls: usize,
    model: String,
    provider: String,
}

impl Caller<'_> {
    async fn ask(&mut self, system: &str, user: &str) -> Result<String, TotError> {
        let msgs = vec![json!({"role":"system","content": system}), json!({"role":"user","content": user})];
        self.calls += 1;
        let a = self.llm.chat_messages(&msgs, None, Some(self.max_tokens), false).await.map_err(TotError::NoModel)?;
        self.model = a.model;
        self.provider = a.provider;
        Ok(visible_text(a.message["content"].as_str().unwrap_or("")))
    }
}

const PROPOSE: &str = "ROLE: PROPOSE. You are the thought generator of a tree-of-thought reasoner. Given a problem and the reasoning so far, propose distinct, concrete next steps (each one or two sentences, a different approach from the others). Reply with ONLY a JSON array of strings.";
const SCORE: &str = "ROLE: SCORE. You are the evaluator of a tree-of-thought reasoner. Rate how likely the latest step is to lead to a correct, complete solution of the problem, from 0 (wrong or useless) to 10 (certainly right and on track). Be critical. Reply with ONLY JSON: {\"score\": <0-10>, \"reason\": \"<one sentence>\"}.";
const ANSWER: &str = "ROLE: ANSWER. Solve the problem for the user. The reasoning path is your outline: follow its steps in order and turn each into concrete, specific content (numbers, settings, times, choices) so the result can be acted on. Do not review or grade the path, and do not say it is incomplete; fill the gaps yourself. Only if a fact is truly unknowable without more information, state the assumption you made in one short line.";

/// The JSON array of thoughts in a model reply (or a bullet/numbered list as a fallback).
pub fn parse_thoughts(text: &str) -> Vec<String> {
    if let (Some(a), Some(b)) = (text.find('['), text.rfind(']')) {
        if a < b {
            if let Ok(Value::Array(v)) = serde_json::from_str::<Value>(&text[a..=b]) {
                let out: Vec<String> = v.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect();
                if !out.is_empty() {
                    return out;
                }
            }
        }
    }
    text.lines()
        .filter_map(|l| {
            let t = l.trim();
            let t = t.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['.', ')']).trim_start();
            t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or(if t.len() < l.trim().len() { Some(t) } else { None }).map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// (score 0-10, reason) from a scorer reply: JSON, or the first number in the text.
pub fn parse_score(text: &str) -> Option<(f64, String)> {
    if let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str::<Value>(&text[a..=b]) {
                let s = v["score"].as_f64().or_else(|| v["score"].as_str().and_then(|x| x.trim().parse().ok()));
                if let Some(s) = s {
                    return Some((s.clamp(0.0, 10.0), v["reason"].as_str().unwrap_or("").to_string()));
                }
            }
        }
    }
    let num: String = text.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    num.parse::<f64>().ok().filter(|n| (0.0..=10.0).contains(n)).map(|n| (n, text.trim().chars().take(200).collect()))
}

fn path_text(nodes: &[Node], mut id: Option<usize>) -> Vec<String> {
    let mut v = Vec::new();
    while let Some(i) = id {
        v.push(nodes[i].hypothesis.clone());
        id = nodes[i].parent;
    }
    v.reverse();
    v
}

fn numbered(steps: &[String]) -> String {
    if steps.is_empty() {
        "(no steps yet)".into()
    } else {
        steps.iter().enumerate().map(|(i, s)| format!("{}. {}", i + 1, s)).collect::<Vec<_>>().join("\n")
    }
}

pub async fn reason(llm: &LemonadeModule, problem: &str, p: &TotParams) -> Result<TotResult, TotError> {
    let mut c = Caller { llm, max_tokens: p.max_tokens, calls: 0, model: String::new(), provider: String::new() };
    let mut nodes: Vec<Node> = Vec::new();
    let mut frontier: Vec<Option<usize>> = vec![None];
    let mut exhausted = false;
    'levels: for depth in 1..=p.depth {
        let mut level: Vec<usize> = Vec::new();
        for parent in &frontier {
            // Keep one call for the final answer; a proposal needs `branch` scores after it.
            if c.calls + 1 + p.branch + 1 > p.budget {
                exhausted = true;
                break 'levels;
            }
            let so_far = path_text(&nodes, *parent);
            let reply = c.ask(PROPOSE, &format!("Problem:\n{problem}\n\nReasoning so far:\n{}\n\nPropose {} different next steps.", numbered(&so_far), p.branch)).await?;
            let mut thoughts = parse_thoughts(&reply);
            thoughts.truncate(p.branch);
            if thoughts.is_empty() {
                if nodes.is_empty() {
                    return Err(TotError::Unusable(format!("the model returned no usable thoughts: {}", reply.chars().take(200).collect::<String>())));
                }
                continue;
            }
            for (k, t) in thoughts.into_iter().enumerate() {
                if c.calls + 1 + 1 > p.budget {
                    exhausted = true;
                    break;
                }
                let mut path = so_far.clone();
                path.push(t.clone());
                let reply = c.ask(SCORE, &format!("Problem:\n{problem}\n\nReasoning path (the last step is the one to rate):\n{}", numbered(&path))).await?;
                let (score, reason) = match parse_score(&reply) {
                    Some((s, r)) => (s / 10.0, if r.is_empty() { "no reason given".to_string() } else { r }),
                    None => (0.0, format!("the model gave no usable score: {}", reply.chars().take(120).collect::<String>())),
                };
                let id = nodes.len();
                nodes.push(Node { id, parent: *parent, depth, label: format!("Step {depth}.{}", k + 1), hypothesis: t, score, status: "pruned".into(), verifier_checks: vec![format!("self-score {:.0}/10: {reason}", score * 10.0)] });
                level.push(id);
            }
        }
        if level.is_empty() {
            break;
        }
        level.sort_by(|a, b| nodes[*b].score.partial_cmp(&nodes[*a].score).unwrap_or(std::cmp::Ordering::Equal));
        for id in level.iter().take(p.beam) {
            nodes[*id].status = "promoted".into();
        }
        frontier = level.into_iter().take(p.beam).map(Some).collect();
        if exhausted {
            break;
        }
    }
    if nodes.is_empty() {
        return Err(TotError::Unusable(format!("the budget of {} model calls is too small for even one step", p.budget)));
    }
    // Best path: the best-scored node at the deepest level reached (ties: higher score wins).
    let deepest = nodes.iter().map(|n| n.depth).max().unwrap();
    let best = nodes.iter().filter(|n| n.depth == deepest).max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal)).map(|n| n.id).unwrap();
    let plan = path_text(&nodes, Some(best));
    let mut cur = Some(best);
    while let Some(i) = cur {
        nodes[i].status = "winning".into();
        cur = nodes[i].parent;
    }
    let answer = c.ask(ANSWER, &format!("Problem:\n{problem}\n\nReasoning path:\n{}", numbered(&plan))).await?;
    Ok(TotResult {
        ok: true,
        problem: problem.to_string(),
        nodes,
        winning_branch_id: Some(best),
        winning_plan: plan,
        answer,
        model_calls: c.calls,
        budget_exhausted: exhausted,
        scored_by: "the same model that proposed the steps (self-evaluation, not verification)",
        model: c.model,
        provider: c.provider,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thought_parsing() {
        assert_eq!(parse_thoughts("Sure: [\"a\", \"b\"] done"), vec!["a", "b"]);
        assert_eq!(parse_thoughts("1. first\n2) second\n- third"), vec!["first", "second", "third"]);
        assert!(parse_thoughts("I cannot").is_empty());
    }

    #[test]
    fn score_parsing() {
        assert_eq!(parse_score("{\"score\": 7, \"reason\": \"ok\"}"), Some((7.0, "ok".into())));
        assert_eq!(parse_score("{\"score\": \"8.5\"}").unwrap().0, 8.5);
        assert_eq!(parse_score("I'd say 6 out of 10").unwrap().0, 6.0);
        assert_eq!(parse_score("{\"score\": 40}").unwrap().0, 10.0);
        assert!(parse_score("no idea").is_none());
    }

    #[test]
    fn params_are_clamped() {
        let p = TotParams::from_json(&json!({"branch": 99, "beam": 0, "depth": 50, "budget": 1}));
        assert_eq!((p.branch, p.beam, p.depth, p.budget), (5, 1, 4, 4));
    }
}
