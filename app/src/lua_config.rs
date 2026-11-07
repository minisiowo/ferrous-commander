//! Lua data-table configuration. Deliberately does not execute configuration code.
use serde_json::{Map, Value};

pub fn encode(value: &Value) -> String {
    fn string(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if c.is_ascii_control() => out.push_str(&format!("\\{:03}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"'); out
    }
    fn write(v: &Value, depth: usize) -> String {
        let indent = "  ".repeat(depth + 1);
        let rows: Vec<String> = match v {
            Value::Object(map) => map.iter().map(|(k,v)| {
                let key = if !k.is_empty() && k.bytes().enumerate().all(|(i,b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
                    && !["and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while"].contains(&k.as_str()) {
                    k.clone()
                } else { format!("[{}]", string(k)) };
                format!("{indent}{key} = {},", write(v, depth + 1))
            }).collect(),
            Value::Array(items) => items.iter().map(|v| format!("{indent}{},", write(v, depth + 1))).collect(),
            Value::String(s) => return string(s),
            Value::Null => return "nil".into(),
            _ => return v.to_string(),
        };
        if rows.is_empty() { "{}".into() }
        else { format!("{{\n{}\n{}}}", rows.join("\n"), "  ".repeat(depth)) }
    }
    format!("-- Ferrous Commander settings. Close the app before editing.\n-- Lua data tables, strings, numbers and booleans; scripts are not executed.\nreturn {}\n", write(value, 0))
}

pub fn decode(text: &str) -> Result<Value, String> {
    struct Parser { chars: Vec<char>, pos: usize }
    impl Parser {
        fn peek(&self) -> char { self.chars.get(self.pos).copied().unwrap_or('\0') }
        fn starts(&self, s: &str) -> bool { self.chars[self.pos..].iter().copied().take(s.chars().count()).eq(s.chars()) }
        fn skip(&mut self) -> Result<(), String> {
            loop {
                while self.peek().is_whitespace() { self.pos += 1; }
                if !self.starts("--") { return Ok(()); }
                self.pos += 2;
                if self.starts("[[") {
                    self.pos += 2;
                    while !self.starts("]]") {
                        if self.peek() == '\0' { return Err("Unclosed Lua comment".into()); }
                        self.pos += 1;
                    }
                    self.pos += 2;
                } else { while !['\n', '\0'].contains(&self.peek()) { self.pos += 1; } }
            }
        }
        fn take(&mut self, c: char) -> Result<(), String> {
            self.skip()?;
            if self.peek() != c { return Err(format!("Expected {c:?} at character {}", self.pos)); }
            self.pos += 1; Ok(())
        }
        fn word(&mut self) -> String {
            let start = self.pos;
            while self.peek() == '_' || self.peek().is_ascii_alphanumeric() { self.pos += 1; }
            self.chars[start..self.pos].iter().collect()
        }
        fn string(&mut self) -> Result<String, String> {
            let quote = self.peek(); self.pos += 1;
            let mut out = String::new();
            while self.peek() != quote {
                let c = self.peek(); self.pos += 1;
                match c {
                    '\0' | '\n' | '\r' => return Err("Unclosed Lua string".into()),
                    '\\' => {
                        let c = self.peek(); self.pos += 1;
                        out.push(match c {
                            'n' => '\n', 'r' => '\r', 't' => '\t', 'a' => '\x07', 'b' => '\x08', 'f' => '\x0c', 'v' => '\x0b',
                            '\\' | '\'' | '"' => c,
                            '0'..='9' => {
                                let mut n = c.to_digit(10).unwrap();
                                for _ in 0..2 { if let Some(d) = self.peek().to_digit(10) { n = n * 10 + d; self.pos += 1; } else { break; } }
                                if n > 255 { return Err("Lua decimal escape exceeds 255".into()); }
                                char::from_u32(n).unwrap()
                            }
                            _ => return Err("Unsupported Lua string escape".into()),
                        });
                    }
                    _ => out.push(c),
                }
            }
            self.pos += 1; Ok(out)
        }
        fn value(&mut self, depth: usize) -> Result<Value, String> {
            if depth > 64 { return Err("Lua table nesting exceeds 64".into()); }
            self.skip()?;
            match self.peek() {
                '\'' | '"' => Ok(Value::String(self.string()?)),
                '{' => {
                    self.pos += 1;
                    let mut map = Map::new(); let mut array = Vec::new();
                    loop {
                        self.skip()?;
                        if self.peek() == '}' { self.pos += 1; break; }
                        let key = if self.peek() == '[' {
                            self.pos += 1; self.skip()?;
                            if !['\'', '"'].contains(&self.peek()) { return Err("Table keys must be strings".into()); }
                            let key = self.string()?; self.take(']')?; self.take('=')?; Some(key)
                        } else if self.peek().is_ascii_alphabetic() || self.peek() == '_' {
                            let start = self.pos; let key = self.word(); self.skip()?;
                            if self.peek() == '=' { self.pos += 1; Some(key) } else { self.pos = start; None }
                        } else { None };
                        let value = self.value(depth + 1)?;
                        if let Some(key) = key { map.insert(key, value); } else { array.push(value); }
                        self.skip()?;
                        if [',', ';'].contains(&self.peek()) { self.pos += 1; }
                        else if self.peek() != '}' { return Err(format!("Expected comma or closing brace at character {}", self.pos)); }
                    }
                    if !map.is_empty() && !array.is_empty() { return Err("Mixed named and list entries are unsupported".into()); }
                    Ok(if array.is_empty() { Value::Object(map) } else { Value::Array(array) })
                }
                '-' | '+' | '.' | '0'..='9' => {
                    let start = self.pos;
                    while self.peek().is_ascii_digit() || ['-', '+', '.', 'e', 'E'].contains(&self.peek()) { self.pos += 1; }
                    let s: String = self.chars[start..self.pos].iter().collect();
                    serde_json::from_str(s.strip_prefix('+').unwrap_or(&s)).map_err(|e| e.to_string())
                }
                _ => match self.word().as_str() {
                    "true" => Ok(Value::Bool(true)), "false" => Ok(Value::Bool(false)), "nil" => Ok(Value::Null),
                    _ => Err(format!("Expected a Lua data value at character {} (scripts are not supported)", self.pos)),
                },
            }
        }
    }
    // Bound parsing work and allocations even for accidentally oversized configuration files.
    if text.len() > 8 * 1024 * 1024 { return Err("Configuration exceeds 8 MiB".into()); }
    let mut p = Parser { chars: text.trim_start_matches('\u{feff}').chars().collect(), pos: 0 };
    p.skip()?;
    if p.word() != "return" { return Err("Lua configuration must start with return { ... }".into()); }
    let mut value = p.value(0)?; p.skip()?;
    if p.peek() == ';' { p.pos += 1; p.skip()?; }
    if p.peek() != '\0' { return Err("Unexpected text after Lua settings table".into()); }
    if !value.is_object() { return Err("Lua configuration must return a settings table".into()); }
    // Empty Lua tables represent both maps and lists; use the settings field schema.
    fn lists(v: &mut Value) {
        match v {
            Value::Object(map) => for (key, value) in map {
                if ["favorites", "history", "sidebar_order", "removed_groups", "panes", "paths", "tabs"].contains(&key.as_str()) && value.as_object().is_some_and(|m| m.is_empty()) { *value = Value::Array(Vec::new()); }
                lists(value);
            },
            Value::Array(array) => for value in array { lists(value); },
            _ => {},
        }
    }
    lists(&mut value); Ok(value)
}
