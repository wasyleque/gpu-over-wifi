//! Wyrażenia długości z `vk.xml` (pole `len`): `count`, `count,null-terminated`, `null-terminated`,
//! `(a + 31) / 32`, `latexmath:[\textrm{codeSize} \over 4]` itp. Wartości członków dostarcza wołający.

use crate::CodecError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LenSpec {
    /// Liczba elementów; `None` dla samego `null-terminated` (pojedynczy napis).
    pub count: Option<u64>,
    /// Elementy są napisami zakończonymi zerem (`char**`) albo to pojedynczy napis.
    pub null_terminated: bool,
}

/// Zamienia notację latexmath na zwykłe wyrażenie arytmetyczne.
fn normalize(expr: &str) -> String {
    let mut s = expr.trim().to_string();
    if let Some(r) = s.strip_prefix("latexmath:[") {
        s = r.strip_suffix(']').unwrap_or(r).to_string();
    }
    for f in ["\\textrm{", "\\mathit{", "\\mathtt{"] {
        while let Some(i) = s.find(f) {
            let Some(j) = s[i..].find('}') else { break };
            let inner = s[i + f.len()..i + j].to_string();
            s.replace_range(i..i + j + 1, &inner);
        }
    }
    s = s.replace("\\_", "_").replace("\\times", "*");
    while let Some(i) = s.find("\\lceil{") {
        let Some(j) = s[i..].find("}\\rceil") else { break };
        let inner = s[i + 7..i + j].to_string();
        let r = match inner.split_once("\\over") {
            Some((a, b)) => format!("(({})+({})-1)/({})", a.trim(), b.trim(), b.trim()),
            None => inner,
        };
        s.replace_range(i..i + j + 7, &r);
    }
    if let Some((a, b)) = s.split_once("\\over") {
        s = format!("({})/({})", a.trim(), b.trim());
    }
    s
}

#[derive(Debug, PartialEq)]
enum Tok {
    Num(u64),
    Id(String),
    Op(char),
}

fn tokenize(s: &str) -> Result<Vec<Tok>, CodecError> {
    let mut out = Vec::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let st = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push(Tok::Num(b[st..i].iter().collect::<String>().parse().map_err(|_| CodecError::BadLen("liczba"))?));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            out.push(Tok::Id(b[st..i].iter().collect()));
        } else if "+-*/()".contains(c) {
            out.push(Tok::Op(c));
            i += 1;
        } else {
            return Err(CodecError::BadLen("nieobsługiwany znak (np. -> z parametru polecenia)"));
        }
    }
    Ok(out)
}

struct Parser<'a> {
    t: Vec<Tok>,
    i: usize,
    get: &'a dyn Fn(&str) -> Option<u64>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn expr(&mut self) -> Result<u64, CodecError> {
        let mut v = self.term()?;
        while let Some(Tok::Op(c @ ('+' | '-'))) = self.peek() {
            let c = *c;
            self.i += 1;
            let r = self.term()?;
            v = if c == '+' { v.checked_add(r) } else { v.checked_sub(r) }.ok_or(CodecError::BadLen("przepełnienie"))?;
        }
        Ok(v)
    }
    fn term(&mut self) -> Result<u64, CodecError> {
        let mut v = self.atom()?;
        while let Some(Tok::Op(c @ ('*' | '/'))) = self.peek() {
            let c = *c;
            self.i += 1;
            let r = self.atom()?;
            v = if c == '*' { v.checked_mul(r) } else { v.checked_div(r) }.ok_or(CodecError::BadLen("przepełnienie lub dzielenie przez zero"))?;
        }
        Ok(v)
    }
    fn atom(&mut self) -> Result<u64, CodecError> {
        let t = self.t.get(self.i).ok_or(CodecError::BadLen("urwane wyrażenie"))?;
        self.i += 1;
        match t {
            Tok::Num(n) => Ok(*n),
            Tok::Id(name) => (self.get)(name).ok_or(CodecError::BadLen("nieznany identyfikator")),
            Tok::Op('(') => {
                let v = self.expr()?;
                match self.t.get(self.i) {
                    Some(Tok::Op(')')) => {
                        self.i += 1;
                        Ok(v)
                    }
                    _ => Err(CodecError::BadLen("brak nawiasu")),
                }
            }
            Tok::Op(_) => Err(CodecError::BadLen("nieoczekiwany operator")),
        }
    }
}

/// Stałe z `vk.xml` używane w wyrażeniach długości.
pub fn constant(name: &str) -> Option<u64> {
    match name {
        "VK_UUID_SIZE" => Some(16),
        "VK_LUID_SIZE" => Some(8),
        _ => None,
    }
}

/// Ocenia wyrażenie `len`. `get` zwraca wartość członka struktury o danej nazwie.
pub fn eval_len(len: &str, get: &dyn Fn(&str) -> Option<u64>) -> Result<LenSpec, CodecError> {
    let (count_part, nt) = match len.split_once(',') {
        Some((a, b)) if b.trim() == "null-terminated" => (Some(a), true),
        Some(_) => return Err(CodecError::BadLen("lista wyrażeń")),
        None if len.trim() == "null-terminated" => (None, true),
        None => (Some(len), false),
    };
    let count = match count_part {
        None => None,
        Some(e) => {
            let n = normalize(e);
            let lookup = |name: &str| constant(name).or_else(|| get(name));
            let mut p = Parser { t: tokenize(&n)?, i: 0, get: &lookup };
            let v = p.expr()?;
            if p.i != p.t.len() {
                return Err(CodecError::BadLen("nadmiar tokenów"));
            }
            Some(v)
        }
    };
    Ok(LenSpec { count, null_terminated: nt })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(name: &str) -> Option<u64> {
        match name {
            "enabledLayerCount" => Some(3),
            "codeSize" => Some(40),
            "samples" => Some(33),
            "rasterizationSamples" => Some(64),
            _ => None,
        }
    }

    #[test]
    fn simple_and_null_terminated_forms() {
        assert_eq!(eval_len("enabledLayerCount", &vals).unwrap(), LenSpec { count: Some(3), null_terminated: false });
        assert_eq!(eval_len("enabledLayerCount,null-terminated", &vals).unwrap(), LenSpec { count: Some(3), null_terminated: true });
        assert_eq!(eval_len("null-terminated", &vals).unwrap(), LenSpec { count: None, null_terminated: true });
    }

    #[test]
    fn arithmetic_and_latexmath_forms() {
        assert_eq!(eval_len("(rasterizationSamples + 31) / 32", &vals).unwrap().count, Some(2));
        assert_eq!(eval_len("latexmath:[\\textrm{codeSize} \\over 4]", &vals).unwrap().count, Some(10));
        assert_eq!(eval_len("latexmath:[\\lceil{\\mathit{samples} \\over 32}\\rceil]", &vals).unwrap().count, Some(2));
        assert_eq!(eval_len("latexmath:[\\lceil{\\mathit{rasterizationSamples} \\over 32}\\rceil]", &vals).unwrap().count, Some(2));
        assert_eq!(eval_len("latexmath:[2 \\times \\mathtt{VK\\_UUID\\_SIZE}]", &vals).unwrap().count, Some(32));
    }

    #[test]
    fn bad_expressions_are_errors_not_panics() {
        for e in ["unknownMember", "pAllocateInfo->descriptorSetCount", "a,b", "(codeSize", "codeSize +", "codeSize / 0", "", "4 4"] {
            assert!(eval_len(e, &vals).is_err(), "{e:?}");
        }
        assert!(eval_len("codeSize - 41", &vals).is_err(), "niedomiar");
    }

    #[test]
    fn every_len_expression_in_vk_xml_is_either_supported_or_a_known_command_form() {
        let xml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../third_party/vk.xml")).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        let mut rest = xml.as_str();
        while let Some(i) = rest.find(" len=\"") {
            let r = &rest[i + 6..];
            let j = r.find('"').unwrap();
            seen.insert(r[..j].replace("&gt;", ">").replace("&lt;", "<"));
            rest = &r[j..];
        }
        let any = |_: &str| Some(7u64);
        let mut unsupported = Vec::new();
        for e in &seen {
            if eval_len(e, &any).is_err() {
                unsupported.push(e.clone());
            }
        }
        // Dopuszczalne: tylko formy z parametrów poleceń (`->`) i listy wyrażeń.
        let bad: Vec<_> = unsupported.iter().filter(|e| !e.contains("->") && !e.contains(',')).collect();
        assert!(bad.is_empty(), "nieobsługiwane wyrażenia struktur: {bad:?}");
        assert!(seen.len() > 150, "{}", seen.len());
    }
}
