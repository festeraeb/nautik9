//! Template notes: notes under `Templates/` with variables, used to create new notes.
//!
//! Jade Vault documents Templater-style templates; this is a small, fixed subset with no code
//! execution (use a note script for logic):
//!
//! - `<% tp.date.now("YYYY-MM-DD") %>`, `<% tp.date.now() %>` (date, `YYYY-MM-DD`), and
//!   `<% tp.date.now("HH:mm") %>` for time; format tokens YYYY YY MMMM MMM MM M DD D dddd ddd
//!   HH H hh mm ss A a;
//! - `<% tp.file.title %>` the new note's title (last path part, no `.md`);
//! - `<% tp.file.cursor() %>` marks where the cursor goes: removed from the text, its offset is
//!   reported; the first one wins and later ones are removed too;
//! - `<% tp.system.prompt("Question?") %>` filled from the `values` map you pass (key = the
//!   question text); a missing value is an error that lists the open questions, never a guess;
//! - `{{date}}`, `{{time}}`, `{{title}}` (Obsidian core style);
//! - `-%>` closes a tag and removes the line break after it.
//!
//! Any other `<% ... %>` is an error naming it: nothing is silently dropped or evaluated.

use async_trait::async_trait;
use chrono::{DateTime, Local};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::tools::{note_by_path, write_note, Tool, ToolCtx};

pub const TEMPLATE_DIR: &str = "Templates/";

#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub text: String,
    /// Character offset of the cursor marker in `text`.
    pub cursor: Option<usize>,
    /// Prompt questions the template asks, in order (answered or not).
    pub prompts: Vec<String>,
}

/// Translate a moment-style date format to text.
pub fn format_date(fmt: &str, now: &DateTime<Local>) -> String {
    const TOKENS: [(&str, &str); 17] = [
        ("YYYY", "%Y"),
        ("MMMM", "%B"),
        ("dddd", "%A"),
        ("MMM", "%b"),
        ("ddd", "%a"),
        ("YY", "%y"),
        ("MM", "%m"),
        ("DD", "%d"),
        ("HH", "%H"),
        ("hh", "%I"),
        ("mm", "%M"),
        ("ss", "%S"),
        ("M", "%-m"),
        ("D", "%-d"),
        ("H", "%-H"),
        ("A", "%p"),
        ("a", "%P"),
    ];
    let mut out = String::new();
    let mut rest = fmt;
    'outer: while !rest.is_empty() {
        for (tok, f) in TOKENS {
            if let Some(r) = rest.strip_prefix(tok) {
                out.push_str(&now.format(f).to_string());
                rest = r;
                continue 'outer;
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn parse_str_arg(s: &str) -> Option<String> {
    let t = s.trim();
    for q in ['"', '\''] {
        if t.len() >= 2 && t.starts_with(q) && t.ends_with(q) {
            return Some(t[1..t.len() - 1].to_string());
        }
    }
    None
}

/// Fill a template. `values` answers `tp.system.prompt` questions.
pub fn render(template: &str, title: &str, values: &HashMap<String, String>, now: &DateTime<Local>) -> Result<Rendered, String> {
    let title_only = title.trim_end_matches(".md").rsplit('/').next().unwrap_or(title).to_string();
    let mut out = String::new();
    let mut prompts = Vec::new();
    let mut missing = Vec::new();
    let mut marked = false;
    let mut rest = template;
    while let Some(i) = rest.find("<%") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let end = after.find("%>").ok_or("template: `<%` without a closing `%>`")?;
        let mut inner = after[..end].trim();
        let mut trim_nl = false;
        if let Some(x) = inner.strip_suffix('-') {
            inner = x.trim();
            trim_nl = true;
        }
        if inner.starts_with('*') {
            return Err("template: `<%* code %>` blocks are not supported (use a note script)".into());
        }
        let inner = inner.strip_prefix("await ").unwrap_or(inner).trim();
        let piece = if inner == "tp.file.title" {
            title_only.clone()
        } else if inner == "tp.date.now()" || inner == "tp.date.now" {
            format_date("YYYY-MM-DD", now)
        } else if let Some(a) = inner.strip_prefix("tp.date.now(").and_then(|x| x.strip_suffix(')')) {
            let f = parse_str_arg(a).ok_or_else(|| format!("template: bad format in `{inner}`"))?;
            format_date(&f, now)
        } else if inner == "tp.file.cursor()" || inner == "tp.file.cursor" {
            // A marker char, resolved after {{vars}} so the offset stays exact.
            if marked { String::new() } else { marked = true; "\u{1}".to_string() }
        } else if let Some(a) = inner.strip_prefix("tp.system.prompt(").and_then(|x| x.strip_suffix(')')) {
            let q = a.split(',').next().and_then(parse_str_arg).ok_or_else(|| format!("template: bad prompt in `{inner}`"))?;
            if !prompts.contains(&q) {
                prompts.push(q.clone());
            }
            match values.get(&q) {
                Some(v) => v.clone(),
                None => {
                    if !missing.contains(&q) {
                        missing.push(q);
                    }
                    String::new()
                }
            }
        } else {
            return Err(format!("template: `<% {inner} %>` is not supported"));
        };
        out.push_str(&piece);
        rest = &after[end + 2..];
        if trim_nl {
            rest = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n')).unwrap_or(rest);
        }
    }
    out.push_str(rest);
    if !missing.is_empty() {
        return Err(format!("the template asks questions with no value: {}. Pass them in `values` (key = the question text).", missing.iter().map(|q| format!("\"{q}\"")).collect::<Vec<_>>().join(", ")));
    }
    let mut out = out.replace("{{date}}", &format_date("YYYY-MM-DD", now)).replace("{{time}}", &format_date("HH:mm", now)).replace("{{title}}", &title_only);
    let cursor = out.find('\u{1}').map(|pos| out[..pos].chars().count());
    out.retain(|c| c != '\u{1}');
    Ok(Rendered { cursor, text: out, prompts })
}

/// All templates (note paths under `Templates/`).
pub async fn list(ctx: &ToolCtx) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = ctx.vault.list_items().await.map_err(|e| e.to_string())?.into_iter().map(|n| n.title).filter(|t| t.trim_start_matches('/').starts_with(TEMPLATE_DIR)).collect();
    v.sort();
    Ok(v)
}

fn template_path(name: &str) -> String {
    let n = name.trim().trim_start_matches('/');
    let n = if n.starts_with(TEMPLATE_DIR) { n.to_string() } else { format!("{TEMPLATE_DIR}{n}") };
    if n.ends_with(".md") {
        n
    } else {
        format!("{n}.md")
    }
}

/// Create a note from a template. Refuses to overwrite an existing note.
pub async fn create(ctx: &ToolCtx, template: &str, title: &str, values: &HashMap<String, String>, dry_run: bool) -> Result<(String, Rendered), String> {
    let tp = template_path(template);
    let t = note_by_path(&ctx.vault, &tp).await?.ok_or_else(|| format!("no template `{tp}` (use action list)"))?;
    let title = title.trim().trim_start_matches('/');
    if title.is_empty() {
        return Err("empty title".into());
    }
    let path = if title.ends_with(".md") { title.to_string() } else { format!("{title}.md") };
    let r = render(&t.content, &path, values, &Local::now())?;
    if dry_run {
        return Ok((path, r));
    }
    if note_by_path(&ctx.vault, &path).await?.is_some() {
        return Err(format!("a note `{path}` already exists; choose another title"));
    }
    write_note(&ctx.vault, &path, &r.text).await?;
    Ok((path, r))
}

pub struct TemplateTool;

fn values_of(a: &Value) -> HashMap<String, String> {
    a["values"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect()).unwrap_or_default()
}

#[async_trait]
impl Tool for TemplateTool {
    fn name(&self) -> &str {
        "template"
    }
    fn description(&self) -> &str {
        "Template notes in Templates/ with variables (<% tp.date.now(\"YYYY-MM-DD\") %>, tp.file.title, tp.file.cursor(), tp.system.prompt(\"Question?\")). Actions: list, show (template), preview (template, title, values?: renders without saving), create (template, title, values?: makes the note; never overwrites). `values` maps each prompt question to its answer."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"action":{"type":"string","enum":["list","show","preview","create"]},"template":{"type":"string"},"title":{"type":"string","description":"path/title of the new note"},"values":{"type":"object","description":"answers to the template's prompt questions"}},"required":["action"]})
    }
    async fn call(&self, a: &Value, ctx: &ToolCtx) -> Result<String, String> {
        let tpl = || a["template"].as_str().ok_or_else(|| "missing string argument `template`".to_string());
        let title = || a["title"].as_str().ok_or_else(|| "missing string argument `title`".to_string());
        match a["action"].as_str().ok_or("missing string argument `action`")? {
            "list" => {
                let l = list(ctx).await?;
                Ok(if l.is_empty() { "(no templates; make a note under Templates/)".into() } else { l.join("\n") })
            }
            "show" => {
                let tp = template_path(tpl()?);
                Ok(note_by_path(&ctx.vault, &tp).await?.ok_or_else(|| format!("no template `{tp}`"))?.content)
            }
            "preview" => {
                let (path, r) = create(ctx, tpl()?, title()?, &values_of(a), true).await?;
                Ok(format!("would create `{path}` (cursor at {:?}):\n{}", r.cursor, r.text))
            }
            "create" => {
                let (path, r) = create(ctx, tpl()?, title()?, &values_of(a), false).await?;
                Ok(format!("created `{path}` from the template{}", r.cursor.map(|c| format!("; cursor at character {c}")).unwrap_or_default()))
            }
            other => Err(format!("unknown action `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 29, 14, 5, 9).unwrap()
    }

    #[test]
    fn date_formats() {
        assert_eq!(format_date("YYYY-MM-DD HH:mm:ss", &at()), "2026-10-29 14:05:09");
        assert_eq!(format_date("dddd, MMMM D", &at()), "Thursday, October 29");
        assert_eq!(format_date("hh:mm A", &at()), "02:05 PM");
    }

    #[test]
    fn renders_the_documented_example() {
        let t = "# <% tp.file.title %>\nDate: <% tp.date.now(\"YYYY-MM-DD\") %>\n\n## Focus\n<% await tp.system.prompt(\"What is today's focus?\") %>\n\n## Tasks\n- [ ] <% tp.file.cursor() %>\n";
        let mut v = HashMap::new();
        v.insert("What is today's focus?".to_string(), "dock repairs".to_string());
        let r = render(t, "Daily/2026-10-29.md", &v, &at()).unwrap();
        assert!(r.text.starts_with("# 2026-10-29\nDate: 2026-10-29\n"));
        assert!(r.text.contains("## Focus\ndock repairs\n"));
        assert!(r.text.ends_with("- [ ] \n"));
        let c = r.cursor.unwrap();
        assert_eq!(r.text.chars().skip(c).collect::<String>(), "\n", "cursor sits after `- [ ] `");
        assert_eq!(r.prompts, vec!["What is today's focus?".to_string()]);
    }

    #[test]
    fn missing_prompt_value_is_an_error_not_a_guess() {
        let e = render("<% tp.system.prompt(\"Who?\") %>", "x", &HashMap::new(), &at()).unwrap_err();
        assert!(e.contains("\"Who?\""), "{e}");
    }

    #[test]
    fn unknown_tags_and_code_blocks_are_refused() {
        assert!(render("<% tp.web.daily_quote() %>", "x", &HashMap::new(), &at()).unwrap_err().contains("not supported"));
        assert!(render("<%* tR += 1 %>", "x", &HashMap::new(), &at()).unwrap_err().contains("not supported"));
        assert!(render("<% tp.file.title", "x", &HashMap::new(), &at()).is_err());
    }

    #[test]
    fn trim_and_obsidian_vars() {
        let r = render("a <% tp.file.title -%>\nb {{date}} {{time}} {{title}}", "Notes/Idea.md", &HashMap::new(), &at()).unwrap();
        assert_eq!(r.text, "a Ideab 2026-10-29 14:05 Idea");
    }
}
