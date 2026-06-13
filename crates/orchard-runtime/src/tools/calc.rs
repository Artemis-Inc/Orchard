//! A safe arithmetic expression evaluator for the `calculator` pack. No `eval`;
//! a hand-written recursive-descent parser over a whitelisted grammar (matches
//! v2's AST-restricted evaluator: `+ - * / // % **`, unary, parens, the
//! constants pi/e/tau, and a fixed set of math functions).

pub fn evaluate(expr: &str) -> Result<f64, String> {
    let mut p = Parser {
        chars: expr.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let v = p.expr()?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err("unexpected trailing input".into());
    }
    Ok(v)
}

const MAX_EXPONENT: f64 = 10000.0;

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }
    fn eat(&mut self, s: &str) -> bool {
        let cs: Vec<char> = s.chars().collect();
        if self.chars[self.pos..].starts_with(&cs) {
            self.pos += cs.len();
            true
        } else {
            false
        }
    }

    // expr = term (("+"|"-") term)*
    fn expr(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        loop {
            self.skip_ws();
            if self.eat("+") {
                v += self.term()?;
            } else if self.peek() == Some('-') {
                self.pos += 1;
                v -= self.term()?;
            } else {
                break;
            }
        }
        Ok(v)
    }

    // term = factor (("*"|"//"|"/"|"%") factor)*
    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.factor()?;
        loop {
            self.skip_ws();
            if self.eat("//") {
                let r = self.factor()?;
                if r == 0.0 {
                    return Err("division by zero".into());
                }
                v = (v / r).floor();
            } else if self.eat("*") {
                v *= self.factor()?;
            } else if self.eat("/") {
                let r = self.factor()?;
                if r == 0.0 {
                    return Err("division by zero".into());
                }
                v /= r;
            } else if self.eat("%") {
                let r = self.factor()?;
                if r == 0.0 {
                    return Err("division by zero".into());
                }
                v %= r;
            } else {
                break;
            }
        }
        Ok(v)
    }

    // factor = unary ("**" factor)?   (right-assoc)
    fn factor(&mut self) -> Result<f64, String> {
        let base = self.unary()?;
        self.skip_ws();
        if self.eat("**") {
            let exp = self.factor()?;
            if exp.abs() > MAX_EXPONENT {
                return Err("exponent too large".into());
            }
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<f64, String> {
        self.skip_ws();
        if self.eat("-") {
            return Ok(-self.unary()?);
        }
        if self.eat("+") {
            return self.unary();
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<f64, String> {
        self.skip_ws();
        if self.eat("(") {
            let v = self.expr()?;
            self.skip_ws();
            if !self.eat(")") {
                return Err("expected ')'".into());
            }
            return Ok(v);
        }
        match self.peek() {
            Some(c) if c.is_ascii_digit() || c == '.' => self.number(),
            Some(c) if c.is_ascii_alphabetic() => self.name_or_call(),
            other => Err(format!("unexpected character {other:?}")),
        }
    }

    fn number(&mut self) -> Result<f64, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-')
        {
            // stop exponent-sign munching when not after e/E
            let c = self.peek().unwrap();
            if (c == '+' || c == '-')
                && !matches!(
                    self.chars.get(self.pos.wrapping_sub(1)),
                    Some('e') | Some('E')
                )
            {
                break;
            }
            self.pos += 1;
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        s.parse::<f64>()
            .map_err(|_| format!("invalid number '{s}'"))
    }

    fn name_or_call(&mut self) -> Result<f64, String> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_') {
            self.pos += 1;
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        self.skip_ws();
        if self.eat("(") {
            let mut args = vec![self.expr()?];
            self.skip_ws();
            while self.eat(",") {
                args.push(self.expr()?);
                self.skip_ws();
            }
            if !self.eat(")") {
                return Err("expected ')'".into());
            }
            return call_fn(&name, &args);
        }
        match name.as_str() {
            "pi" => Ok(std::f64::consts::PI),
            "e" => Ok(std::f64::consts::E),
            "tau" => Ok(std::f64::consts::TAU),
            _ => Err(format!("unknown name '{name}'")),
        }
    }
}

fn call_fn(name: &str, args: &[f64]) -> Result<f64, String> {
    let a0 = args.first().copied().unwrap_or(0.0);
    let r = match name {
        "abs" => a0.abs(),
        "round" => a0.round(),
        "sqrt" => a0.sqrt(),
        "sin" => a0.sin(),
        "cos" => a0.cos(),
        "tan" => a0.tan(),
        "log" => a0.ln(),
        "log2" => a0.log2(),
        "log10" => a0.log10(),
        "exp" => a0.exp(),
        "floor" => a0.floor(),
        "ceil" => a0.ceil(),
        "min" => args.iter().copied().fold(f64::INFINITY, f64::min),
        "max" => args.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        _ => return Err(format!("unknown function '{name}'")),
    };
    if r.is_nan() || r.is_infinite() {
        return Err("math error".into());
    }
    Ok(r)
}
