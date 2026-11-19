//! Filter query syntax (modelled on Modern CSV's):
//!
//! * `foo`        cell contains "foo" (case-insensitive)
//! * `foo*bar`    wildcard, whole cell must match
//! * `a | b`      OR,  `a & b`  AND,  `!a`  NOT
//! * `>5` `<=10`  numeric comparison, `50+` (>= 50), `50-` (<= 50), `10-20` inclusive range
//! * `=foo`       cell equals "foo" (case-insensitive); `!` alone = not empty
//! * `#regex`     the whole query is a (case-sensitive) regular expression
//! * `\|` `\&` `\*` escape special characters

use regex::{Regex, RegexBuilder};

pub struct Query {
    groups: Vec<Vec<Term>>, // OR of ANDs
}

struct Term {
    neg: bool,
    pred: Pred,
}

enum Pred {
    Contains(String),
    Exact(String),
    Empty,
    Pattern(Regex),
    Cmp(Op, f64),
    Range(f64, f64),
}

#[derive(Clone, Copy)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
}

impl Query {
    pub fn parse(s: &str) -> Result<Query, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty filter".into());
        }
        if let Some(re) = s.strip_prefix('#') {
            let r = Regex::new(re).map_err(|e| e.to_string())?;
            return Ok(Query { groups: vec![vec![Term { neg: false, pred: Pred::Pattern(r) }]] });
        }
        let mut groups = Vec::new();
        for alt in split_unescaped(s, '|') {
            let mut terms = Vec::new();
            for t in split_unescaped(&alt, '&') {
                let t = t.trim();
                if !t.is_empty() {
                    terms.push(parse_term(t)?);
                }
            }
            if !terms.is_empty() {
                groups.push(terms);
            }
        }
        if groups.is_empty() {
            return Err("empty filter".into());
        }
        Ok(Query { groups })
    }

    /// Positive terms need at least one matching field; negated terms need none.
    pub fn matches<S: AsRef<str>>(&self, fields: &[S]) -> bool {
        self.groups.iter().any(|g| {
            g.iter().all(|t| {
                let hit = fields.iter().any(|f| t.pred.test(f.as_ref()));
                hit != t.neg
            })
        })
    }
}

fn split_unescaped(s: &str, sep: char) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(n) if n == sep => out.last_mut().unwrap().push(n),
                Some(n) => {
                    out.last_mut().unwrap().push('\\');
                    out.last_mut().unwrap().push(n);
                }
                None => out.last_mut().unwrap().push('\\'),
            }
        } else if c == sep {
            out.push(String::new());
        } else {
            out.last_mut().unwrap().push(c);
        }
    }
    out
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn parse_term(t: &str) -> Result<Term, String> {
    let (neg, t) = match t.strip_prefix('!') {
        Some(rest) => (true, rest.trim()),
        None => (false, t),
    };
    let pred = parse_pred(t)?;
    Ok(Term { neg, pred })
}

fn parse_pred(t: &str) -> Result<Pred, String> {
    if t.is_empty() {
        return Ok(Pred::Empty);
    }
    if let Some(rest) = t.strip_prefix('=') {
        return Ok(Pred::Exact(unescape(rest).to_lowercase()));
    }
    for (prefix, op) in [(">=", Op::Ge), ("<=", Op::Le), (">", Op::Gt), ("<", Op::Lt)] {
        if let Some(v) = t.strip_prefix(prefix).and_then(num) {
            return Ok(Pred::Cmp(op, v));
        }
    }
    if let Some(v) = t.strip_suffix('+').and_then(num) {
        return Ok(Pred::Cmp(Op::Ge, v));
    }
    if let Some(v) = t.strip_suffix('-').and_then(num) {
        return Ok(Pred::Cmp(Op::Le, v));
    }
    for (i, ch) in t.char_indices() {
        if ch == '-' && i > 0 {
            if let (Some(a), Some(b)) = (num(&t[..i]), num(&t[i + 1..])) {
                return Ok(Pred::Range(a.min(b), a.max(b)));
            }
        }
    }
    // Wildcards / escapes.
    let mut has_wild = false;
    let mut pattern = String::from("^");
    let mut literal = String::new();
    let mut chars = t.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let n = chars.next().unwrap_or('\\');
                pattern.push_str(&regex::escape(&n.to_string()));
                literal.push(n);
            }
            '*' => {
                has_wild = true;
                pattern.push_str(".*");
            }
            c => {
                pattern.push_str(&regex::escape(&c.to_string()));
                literal.push(c);
            }
        }
    }
    if has_wild {
        pattern.push('$');
        let r = RegexBuilder::new(&pattern).case_insensitive(true).build().map_err(|e| e.to_string())?;
        Ok(Pred::Pattern(r))
    } else {
        Ok(Pred::Contains(literal.to_lowercase()))
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(chars.next().unwrap_or('\\'));
        } else {
            out.push(c);
        }
    }
    out
}

/// Escape a literal value for use in a query term.
pub fn escape_value(v: &str) -> String {
    let mut out = String::new();
    for c in v.chars() {
        if matches!(c, '\\' | '|' | '&' | '*') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn contains_ci(hay: &str, needle_lc: &str) -> bool {
    if needle_lc.is_empty() {
        return true;
    }
    if hay.is_ascii() && needle_lc.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle_lc.as_bytes());
        if n.len() > h.len() {
            return false;
        }
        return h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    hay.to_lowercase().contains(needle_lc)
}

impl Pred {
    fn test(&self, f: &str) -> bool {
        match self {
            Pred::Contains(n) => contains_ci(f, n),
            Pred::Exact(n) => f.to_lowercase() == *n,
            Pred::Empty => f.trim().is_empty(),
            Pred::Pattern(r) => r.is_match(f),
            Pred::Cmp(op, v) => match num(f) {
                Some(x) => match op {
                    Op::Lt => x < *v,
                    Op::Le => x <= *v,
                    Op::Gt => x > *v,
                    Op::Ge => x >= *v,
                },
                None => false,
            },
            Pred::Range(a, b) => num(f).is_some_and(|x| x >= *a && x <= *b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(q: &str, fields: &[&str]) -> bool {
        Query::parse(q).unwrap().matches(fields)
    }

    #[test]
    fn syntax() {
        assert!(m("foo", &["xFOOx"]));
        assert!(!m("!foo", &["xfoox", "bar"]));
        assert!(m("a | b", &["b"]));
        assert!(!m("a & b", &["a"]));
        assert!(m("a & b", &["a", "b"]));
        assert!(m(">5", &["6"]));
        assert!(!m(">5", &["5"]));
        assert!(m("50+", &["50"]));
        assert!(m("10-20", &["15"]));
        assert!(!m("10-20", &["25"]));
        assert!(m("-5", &["-5"]));
        assert!(m("f*r", &["Foobar"]));
        assert!(!m("f*r", &["xfoobar"]));
        assert!(m("#^\\d+$", &["123"]));
        assert!(m("a\\|b", &["a|b"]));
        assert!(m("=abc", &["ABC"]));
        assert!(!m("=ab", &["abc"]));
        assert!(m("!", &["x"]));
        assert!(!m("!", &[""]));
        let v = escape_value("a|b&c*\\");
        assert!(m(&format!("={v}"), &["a|b&c*\\"]));
    }
}
