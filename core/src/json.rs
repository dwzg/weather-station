//! A minimal JSON object writer for the status page.

use std::fmt::{Display, Write};

pub struct JsonObject {
    s: String,
    first: Vec<bool>,
}

impl Default for JsonObject {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonObject {
    pub fn new() -> Self {
        Self { s: String::from("{"), first: vec![true] }
    }

    fn key(&mut self, k: &str) {
        let first = self.first.last_mut().expect("an open object");
        if !*first {
            self.s.push(',');
        }
        *first = false;
        escape_into(&mut self.s, k);
        self.s.push(':');
    }

    pub fn str(&mut self, k: &str, v: &str) -> &mut Self {
        self.key(k);
        escape_into(&mut self.s, v);
        self
    }

    /// A number, or anything else that is already valid JSON.
    pub fn raw(&mut self, k: &str, v: impl Display) -> &mut Self {
        self.key(k);
        let _ = write!(self.s, "{v}");
        self
    }

    pub fn bool(&mut self, k: &str, v: bool) -> &mut Self {
        self.raw(k, v)
    }

    pub fn open(&mut self, k: &str) -> &mut Self {
        self.key(k);
        self.s.push('{');
        self.first.push(true);
        self
    }

    pub fn close(&mut self) -> &mut Self {
        self.s.push('}');
        self.first.pop();
        self
    }

    pub fn finish(mut self) -> String {
        while !self.first.is_empty() {
            self.close();
        }
        self.s
    }
}

/// Writes `v` as a JSON string literal.
pub fn escape_into(out: &mut String, v: &str) {
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_and_escaped() {
        let mut j = JsonObject::new();
        j.str("a", "x\"y\\z\n\u{1}").raw("n", -3).open("o").bool("b", true).raw("f", 1.5).close().str("e", "");
        assert_eq!(j.finish(), r#"{"a":"x\"y\\z\n\u0001","n":-3,"o":{"b":true,"f":1.5},"e":""}"#);
        let mut j = JsonObject::new();
        j.open("x").open("y");
        assert_eq!(j.finish(), r#"{"x":{"y":{}}}"#);
    }
}
