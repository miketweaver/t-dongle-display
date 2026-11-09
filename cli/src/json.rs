//! A minimal JSON value and pretty printer (two-space indent), so `--json`
//! needs no serde.

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn obj(pairs: Vec<(&str, J)>) -> J {
        J::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    pub fn str(s: impl Into<String>) -> J {
        J::Str(s.into())
    }
    pub fn opt<T>(v: Option<T>, f: impl FnOnce(T) -> J) -> J {
        v.map(f).unwrap_or(J::Null)
    }

    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, o: &mut String, ind: usize) {
        match self {
            J::Null => o.push_str("null"),
            J::Bool(b) => o.push_str(if *b { "true" } else { "false" }),
            J::Int(i) => o.push_str(&i.to_string()),
            J::Num(n) if n.is_finite() => {
                let s = format!("{n}");
                o.push_str(&s);
                if !s.contains('.') && !s.contains('e') {
                    o.push_str(".0");
                }
            }
            J::Num(_) => o.push_str("null"),
            J::Str(s) => escape(s, o),
            J::Arr(v) if v.is_empty() => o.push_str("[]"),
            J::Obj(v) if v.is_empty() => o.push_str("{}"),
            J::Arr(v) => {
                o.push_str("[\n");
                for (i, x) in v.iter().enumerate() {
                    o.push_str(&"  ".repeat(ind + 1));
                    x.write(o, ind + 1);
                    o.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
                }
                o.push_str(&"  ".repeat(ind));
                o.push(']');
            }
            J::Obj(v) => {
                o.push_str("{\n");
                for (i, (k, x)) in v.iter().enumerate() {
                    o.push_str(&"  ".repeat(ind + 1));
                    escape(k, o);
                    o.push_str(": ");
                    x.write(o, ind + 1);
                    o.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
                }
                o.push_str(&"  ".repeat(ind));
                o.push('}');
            }
        }
    }
}

fn escape(s: &str, o: &mut String) {
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_prints_nested_values_with_two_space_indent() {
        let j = J::obj(vec![
            ("a", J::Int(1)),
            ("b", J::Arr(vec![J::Bool(true), J::Null])),
            ("c", J::str("q\"\n")),
            ("d", J::Num(35.0)),
        ]);
        assert_eq!(
            j.pretty(),
            "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null\n  ],\n  \"c\": \"q\\\"\\n\",\n  \"d\": 35.0\n}\n"
        );
    }
}
