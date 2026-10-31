//! Formulas for live links (`{"link": {"expr": "..."}}`, see
//! [`crate::link`]): a small arithmetic language over numbers and lists of
//! numbers, read from other nodes' params at the frame being rendered.
//!
//! - Numbers, `frame`, `time` (seconds from the project's first frame:
//!   `(frame - first_frame) / fps`), `fps`, `pi`, `tau`, `e`; lists
//!   `[a, b]`; `v[i]`.
//! - `+ - * / % ^`, comparisons (`< <= > >= == !=`, 1 or 0), `&& || !`.
//!   Lists work per component, and a number goes with every component
//!   (`[1, 2] * 2`).
//! - `node.param`: that param's value now (keyframes evaluated, links
//!   followed); `node.param.x` (or `.y .z .w`, `.r .g .b .a`) one
//!   component. A node whose name isn't a plain identifier is written
//!   quoted: `"plate-2".center`.
//! - Functions: `sin cos tan asin acos atan atan2 sqrt abs floor ceil
//!   round fract sign min max clamp mix smoothstep pow exp log rad deg
//!   length dist if(c, a, b)`; `noise(x[, seed])` (smooth, -1..1),
//!   `noise3([x, y, z][, seed])` (smooth over 3-D space, one number in
//!   -1..1), `random(seed)` (0..1, the same every frame), `wiggle(freq, amp[,
//!   seed])` (`amp × noise(frame × freq)`: `freq` wobbles per frame), and
//!   `at(x, f)`: `x` worked out at frame `f` (`at(t.track1, frame - 2)`).
//!
//! Angles are in radians (`sin(rad(45))`); rotation params are degrees.
//!
//! In a geometry node (Delete's expression; see [`Expr::eval_elem`]), a
//! formula runs once per element (a point, a face, an instance) and reads
//! it by `@` names: `@P` (where it is, in the scene: `[x, y, z]`; `@P.y`
//! one component), `@N` (its normal, where it has one), `@id`, `@index`
//! (its number, from 0), `@count` (how many there are), and any of its
//! attributes as `@name` (a number, or a list for a vector or a color).
//! `random(@id)` gives each its own number that stays with it, and
//! `noise3(@P * 4)` a smooth 3-D noise (-1..1) over the scene. Other
//! nodes' params (`node.param`) are read as they are at the frame being
//! made (once for all the elements); `at()` can't read them at another.

use serde_json::{json, Value};
use std::fmt;

/// A parsed formula.
#[derive(Debug, Clone)]
pub struct Expr(Node);

#[derive(Debug, Clone)]
enum Node {
    Num(f64),
    Frame,
    Time,
    Fps,
    Ref { node: String, param: String },
    /// An element's `@name` (in a geometry node).
    Attr(String),
    List(Vec<Node>),
    Index(Box<Node>, Box<Node>),
    Unary(char, Box<Node>),
    Binary(Bin, Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

/// A value: a number or a list of them.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Num(f64),
    List(Vec<f64>),
}

impl Val {
    pub fn to_json(&self) -> Value {
        match self {
            Val::Num(x) => json!(x),
            Val::List(v) => json!(v),
        }
    }

    /// A JSON number, bool or list of numbers.
    pub fn from_json(v: &Value) -> Option<Val> {
        match v {
            Value::Number(n) => n.as_f64().map(Val::Num),
            Value::Bool(b) => Some(Val::Num(f64::from(u8::from(*b)))),
            Value::Array(a) => a.iter().map(Value::as_f64).collect::<Option<Vec<_>>>().map(Val::List),
            _ => None,
        }
    }

    fn num(&self) -> Result<f64, String> {
        match self {
            Val::Num(x) => Ok(*x),
            Val::List(v) => Err(format!("expected a number, got a list of {}", v.len())),
        }
    }

    fn map(self, f: impl Fn(f64) -> f64) -> Val {
        match self {
            Val::Num(x) => Val::Num(f(x)),
            Val::List(v) => Val::List(v.into_iter().map(f).collect()),
        }
    }
}

/// `f` per component of `a` and `b`; a number goes with every component.
fn zip(a: &Val, b: &Val, f: impl Fn(f64, f64) -> f64) -> Result<Val, String> {
    Ok(match (a, b) {
        (Val::Num(x), Val::Num(y)) => Val::Num(f(*x, *y)),
        (Val::List(v), Val::Num(y)) => Val::List(v.iter().map(|x| f(*x, *y)).collect()),
        (Val::Num(x), Val::List(w)) => Val::List(w.iter().map(|y| f(*x, *y)).collect()),
        (Val::List(v), Val::List(w)) if v.len() == w.len() => Val::List(v.iter().zip(w).map(|(x, y)| f(*x, *y)).collect()),
        (Val::List(v), Val::List(w)) => return Err(format!("lists of {} and {} components", v.len(), w.len())),
    })
}

macro_rules! funcs {
    ($($name:ident = $text:literal, $min:literal..=$max:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq)]
        enum Func { $($name),* }
        impl Func {
            fn from_name(s: &str) -> Option<Func> {
                match s { $($text => Some(Func::$name),)* _ => None }
            }
            fn name(self) -> &'static str {
                match self { $(Func::$name => $text),* }
            }
            fn arity(self) -> (usize, usize) {
                match self { $(Func::$name => ($min, $max)),* }
            }
        }
    };
}

funcs! {
    Sin = "sin", 1..=1; Cos = "cos", 1..=1; Tan = "tan", 1..=1;
    Asin = "asin", 1..=1; Acos = "acos", 1..=1; Atan = "atan", 1..=1; Atan2 = "atan2", 2..=2;
    Sqrt = "sqrt", 1..=1; Abs = "abs", 1..=1; Floor = "floor", 1..=1; Ceil = "ceil", 1..=1;
    Round = "round", 1..=1; Fract = "fract", 1..=1; Sign = "sign", 1..=1;
    Min = "min", 1..=16; Max = "max", 1..=16; Clamp = "clamp", 3..=3; Mix = "mix", 3..=3;
    Smoothstep = "smoothstep", 3..=3; Pow = "pow", 2..=2; Exp = "exp", 1..=1; Log = "log", 1..=1;
    Rad = "rad", 1..=1; Deg = "deg", 1..=1; Length = "length", 1..=1; Dist = "dist", 2..=2;
    If = "if", 3..=3; Noise = "noise", 1..=2; Noise3 = "noise3", 1..=2; Random = "random", 1..=1; Wiggle = "wiggle", 2..=3;
    At = "at", 2..=2;
}

impl fmt::Display for Func {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.name())
    }
}

// ---- Tokens ----

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Str(String),
    /// `@name`.
    Attr(String),
    Sym(&'static str),
}

/// Tokens with their byte ranges in the source.
fn lex(src: &str) -> Result<Vec<(Tok, std::ops::Range<usize>)>, String> {
    const SYMS: [&str; 22] =
        ["<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "^", "<", ">", "!", "(", ")", "[", "]", ",", ".", ";"];
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // A number; ".5" too, but not the "." of "a.b".
        let after_dot_number = c == '.' && b.get(i + 1).is_some_and(u8::is_ascii_digit) && !matches!(out.last(), Some((Tok::Ident(_) | Tok::Str(_) | Tok::Sym(")" | "]"), _)));
        if c.is_ascii_digit() || after_dot_number {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                let mut j = i + 1;
                if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                    j += 1;
                }
                if j < b.len() && b[j].is_ascii_digit() {
                    i = j;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let text = &src[start..i];
            let x = text.parse().map_err(|_| format!("bad number '{text}'"))?;
            out.push((Tok::Num(x), start..i));
        } else if c.is_ascii_alphabetic() || c == '_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push((Tok::Ident(src[start..i].to_string()), start..i));
        } else if c == '@' {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            if i == start + 1 {
                return Err("expected a name after '@' (@P, @id, an attribute's name)".into());
            }
            out.push((Tok::Attr(src[start + 1..i].to_string()), start..i));
        } else if c == '"' {
            let end = src[i + 1..].find('"').ok_or("a quoted name isn't closed")?;
            out.push((Tok::Str(src[i + 1..i + 1 + end].to_string()), start..i + end + 2));
            i += end + 2;
        } else if let Some(s) = SYMS.iter().find(|s| src[i..].starts_with(**s)) {
            i += s.len();
            out.push((Tok::Sym(s), start..i));
        } else {
            let ch = src[i..].chars().next().unwrap_or('?');
            return Err(format!("unexpected '{ch}'"));
        }
    }
    Ok(out)
}

// ---- Parser ----

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn eat(&mut self, sym: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Sym(s)) if *s == sym) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, sym: &str) -> Result<(), String> {
        if self.eat(sym) {
            Ok(())
        } else {
            Err(match self.peek() {
                Some(t) => format!("expected '{sym}', found {}", show(t)),
                None => format!("expected '{sym}' at the end"),
            })
        }
    }

    fn expr(&mut self) -> Result<Node, String> {
        self.binary(0)
    }

    /// Binary operators by precedence level, lowest first; `^` is in
    /// `power`.
    fn binary(&mut self, level: usize) -> Result<Node, String> {
        const LEVELS: [&[(&str, Bin)]; 5] = [
            &[("||", Bin::Or)],
            &[("&&", Bin::And)],
            &[("<=", Bin::Le), (">=", Bin::Ge), ("==", Bin::Eq), ("!=", Bin::Ne), ("<", Bin::Lt), (">", Bin::Gt)],
            &[("+", Bin::Add), ("-", Bin::Sub)],
            &[("*", Bin::Mul), ("/", Bin::Div), ("%", Bin::Rem)],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        'more: loop {
            for (sym, op) in LEVELS[level] {
                if self.eat(sym) {
                    let right = self.binary(level + 1)?;
                    left = Node::Binary(*op, Box::new(left), Box::new(right));
                    continue 'more;
                }
            }
            return Ok(left);
        }
    }

    fn unary(&mut self) -> Result<Node, String> {
        for sym in ["-", "+", "!"] {
            if self.eat(sym) {
                let inner = self.unary()?;
                return Ok(if sym == "+" { inner } else { Node::Unary(sym.chars().next().unwrap(), Box::new(inner)) });
            }
        }
        self.power()
    }

    /// `a ^ b`, right to left; `-2 ^ 2` is -(2²).
    fn power(&mut self) -> Result<Node, String> {
        let base = self.postfix()?;
        if self.eat("^") {
            let exp = self.unary()?;
            return Ok(Node::Binary(Bin::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn postfix(&mut self) -> Result<Node, String> {
        let mut v = self.primary()?;
        while self.eat("[") {
            let i = self.expr()?;
            self.expect("]")?;
            v = Node::Index(Box::new(v), Box::new(i));
        }
        Ok(v)
    }

    fn primary(&mut self) -> Result<Node, String> {
        let Some(tok) = self.peek().cloned() else { return Err("unexpected end".into()) };
        self.pos += 1;
        match tok {
            Tok::Num(x) => Ok(Node::Num(x)),
            Tok::Sym("(") => {
                let v = self.expr()?;
                self.expect(")")?;
                Ok(v)
            }
            Tok::Sym("[") => Ok(Node::List(self.list("]")?)),
            Tok::Attr(name) => {
                let a = Node::Attr(name);
                if self.eat(".") {
                    return self.component(a);
                }
                Ok(a)
            }
            Tok::Str(node) => {
                if !matches!(self.peek(), Some(Tok::Sym("."))) {
                    return Err(format!("\"{node}\" needs a param: \"{node}\".param"));
                }
                self.reference(node)
            }
            Tok::Ident(name) => {
                if self.eat("(") {
                    let f = Func::from_name(&name).ok_or_else(|| format!("unknown function '{name}'"))?;
                    let args = self.list(")")?;
                    let (min, max) = f.arity();
                    if args.len() < min || args.len() > max {
                        let want = if min == max { format!("{min}") } else { format!("{min} to {max}") };
                        return Err(format!("{f}() takes {want} argument{}", if max == 1 { "" } else { "s" }));
                    }
                    return Ok(Node::Call(f, args));
                }
                if matches!(self.peek(), Some(Tok::Sym("."))) {
                    return self.reference(name);
                }
                match name.as_str() {
                    "frame" => Ok(Node::Frame),
                    "time" => Ok(Node::Time),
                    "fps" => Ok(Node::Fps),
                    "pi" => Ok(Node::Num(std::f64::consts::PI)),
                    "tau" => Ok(Node::Num(std::f64::consts::TAU)),
                    "e" => Ok(Node::Num(std::f64::consts::E)),
                    _ => Err(format!("unknown name '{name}' (a node's param is node.param)")),
                }
            }
            t => Err(format!("unexpected {}", show(&t))),
        }
    }

    /// `.param` and an optional `.x`-style component, after a node name.
    fn reference(&mut self, node: String) -> Result<Node, String> {
        self.expect(".")?;
        let Some(Tok::Ident(param)) = self.peek().cloned() else { return Err(format!("expected a param name after '{node}.'")) };
        self.pos += 1;
        let r = Node::Ref { node, param };
        if self.eat(".") {
            return self.component(r);
        }
        Ok(r)
    }

    /// The `x`-style component after a '.' of `v`.
    fn component(&mut self, v: Node) -> Result<Node, String> {
        let Some(Tok::Ident(c)) = self.peek().cloned() else { return Err("expected a component (x, y, z, w or r, g, b, a)".into()) };
        self.pos += 1;
        let i = match c.as_str() {
            "x" | "r" => 0,
            "y" | "g" => 1,
            "z" | "b" => 2,
            "w" | "a" => 3,
            _ => return Err(format!("unknown component '{c}' (x, y, z, w or r, g, b, a)")),
        };
        Ok(Node::Index(Box::new(v), Box::new(Node::Num(i as f64))))
    }

    /// Comma-separated expressions up to `close`.
    fn list(&mut self, close: &str) -> Result<Vec<Node>, String> {
        let mut items = Vec::new();
        if self.eat(close) {
            return Ok(items);
        }
        loop {
            items.push(self.expr()?);
            if self.eat(close) {
                return Ok(items);
            }
            self.expect(",")?;
        }
    }
}

fn show(t: &Tok) -> String {
    match t {
        Tok::Num(x) => format!("{x}"),
        Tok::Ident(s) => format!("'{s}'"),
        Tok::Str(s) => format!("\"{s}\""),
        Tok::Attr(s) => format!("'@{s}'"),
        Tok::Sym(s) => format!("'{s}'"),
    }
}

/// Parse a formula.
pub fn parse(src: &str) -> Result<Expr, String> {
    let toks = lex(src)?.into_iter().map(|(t, _)| t).collect();
    let mut p = Parser { toks, pos: 0 };
    if p.peek().is_none() {
        return Err("empty formula".into());
    }
    let node = p.expr()?;
    match p.peek() {
        None => Ok(Expr(node)),
        Some(t) => Err(format!("unexpected {} after the formula", show(t))),
    }
}

/// The project's timing, for `time` and `fps`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clock {
    pub fps: f64,
    pub first_frame: f64,
}

impl Default for Clock {
    fn default() -> Self {
        Clock { fps: 24.0, first_frame: 1.0 }
    }
}

impl Clock {
    /// Its rate, 24 when it isn't a usable one.
    pub fn rate(&self) -> f64 {
        if self.fps.is_finite() && self.fps > 0.0 {
            self.fps
        } else {
            24.0
        }
    }

    /// Seconds from the first frame to frame `t`.
    pub fn time(&self, t: f64) -> f64 {
        (t - self.first_frame) / self.rate()
    }
}

/// What a formula reads params through: node, param and time to the value
/// there (keyframes evaluated, links followed), or why it can't (the node
/// is gone, links loop).
pub type Lookup<'a> = &'a dyn Fn(&str, &str, f64) -> Result<Value, String>;

/// What a geometry node's formula reads its element through: an `@`
/// name to its value there (None: the element hasn't it).
pub type Elem<'a> = &'a dyn Fn(&str) -> Option<Val>;

impl Expr {
    /// The nodes and params it reads, in order (repeats included).
    pub fn refs(&self) -> Vec<(&str, &str)> {
        fn walk<'a>(n: &'a Node, out: &mut Vec<(&'a str, &'a str)>) {
            match n {
                Node::Ref { node, param } => out.push((node, param)),
                Node::List(v) | Node::Call(_, v) => v.iter().for_each(|x| walk(x, out)),
                Node::Index(a, b) | Node::Binary(_, a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
                Node::Unary(_, a) => walk(a, out),
                Node::Num(_) | Node::Frame | Node::Time | Node::Fps | Node::Attr(_) => {}
            }
        }
        let mut out = Vec::new();
        walk(&self.0, &mut out);
        out
    }

    /// The `@` names it reads, in order (repeats included).
    pub fn attrs(&self) -> Vec<&str> {
        fn walk<'a>(n: &'a Node, out: &mut Vec<&'a str>) {
            match n {
                Node::Attr(name) => out.push(name),
                Node::List(v) | Node::Call(_, v) => v.iter().for_each(|x| walk(x, out)),
                Node::Index(a, b) | Node::Binary(_, a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
                Node::Unary(_, a) => walk(a, out),
                Node::Num(_) | Node::Frame | Node::Time | Node::Fps | Node::Ref { .. } => {}
            }
        }
        let mut out = Vec::new();
        walk(&self.0, &mut out);
        out
    }

    /// Its value at frame `t` (fractional between frames).
    pub fn eval(&self, lookup: Lookup, clock: Clock, t: f64) -> Result<Val, String> {
        eval(&self.0, lookup, clock, t, None)
    }

    /// Its value for one element of a geometry node's (see the module's
    /// notes), at frame `t`: `elem` gives the element's `@` names (None
    /// for one it hasn't), `refs` the `node.param` values it reads (by
    /// "node.param", worked out at `t` beforehand: see [`Expr::refs`]).
    /// Parse once, then run this per element (it's safe from many threads
    /// at once).
    pub fn eval_elem(&self, clock: Clock, t: f64, elem: Elem, refs: &serde_json::Map<String, Value>) -> Result<Val, String> {
        let read: Lookup = &|node, param, at| {
            if at != t {
                return Err(format!("{node}.{param}: a geometry node's expression reads other nodes' params at its own frame only (not through at())"));
            }
            match refs.get(&format!("{node}.{param}")) {
                Some(Value::Null) => Err(format!("{node}.{param}: there's no such node or param")),
                Some(v) => Ok(v.clone()),
                None => Err(format!("{node}.{param} isn't known here")),
            }
        };
        eval(&self.0, read, clock, t, Some(elem))
    }

    /// Whether its value can change from frame to frame (it reads the
    /// frame or time, or wiggles).
    pub fn uses_time(&self) -> bool {
        fn walk(n: &Node) -> bool {
            match n {
                Node::Frame | Node::Time => true,
                Node::Call(Func::Wiggle | Func::At, _) => true,
                Node::List(v) | Node::Call(_, v) => v.iter().any(walk),
                Node::Index(a, b) | Node::Binary(_, a, b) => walk(a) || walk(b),
                Node::Unary(_, a) => walk(a),
                Node::Num(_) | Node::Fps | Node::Ref { .. } | Node::Attr(_) => false,
            }
        }
        walk(&self.0)
    }
}

fn eval(n: &Node, lookup: Lookup, clock: Clock, t: f64, elem: Option<Elem>) -> Result<Val, String> {
    let ev = |n: &Node| eval(n, lookup, clock, t, elem);
    Ok(match n {
        Node::Num(x) => Val::Num(*x),
        Node::Frame => Val::Num(t),
        Node::Time => Val::Num(clock.time(t)),
        Node::Fps => Val::Num(clock.rate()),
        Node::Ref { node, param } => {
            let v = lookup(node, param, t)?;
            Val::from_json(&v).ok_or_else(|| format!("{node}.{param} isn't a number or a list of numbers"))?
        }
        Node::Attr(name) => {
            let elem = elem.ok_or_else(|| format!("@{name} only means something in a geometry node (per point, face or instance)"))?;
            elem(name).ok_or_else(|| format!("no attribute @{name} here"))?
        }
        Node::List(items) => {
            Val::List(items.iter().map(|x| ev(x)?.num().map_err(|e| format!("in a list: {e}"))).collect::<Result<_, _>>()?)
        }
        Node::Index(v, i) => {
            let i = ev(i)?.num()?;
            match ev(v)? {
                Val::List(l) => {
                    let k = i.round();
                    if k < 0.0 || k as usize >= l.len() {
                        return Err(format!("index {k} out of range (it has {} components)", l.len()));
                    }
                    Val::Num(l[k as usize])
                }
                Val::Num(x) if i.round() == 0.0 => Val::Num(x),
                Val::Num(_) => return Err("indexing a number".into()),
            }
        }
        Node::Unary('-', a) => ev(a)?.map(|x| -x),
        Node::Unary(_, a) => ev(a)?.map(|x| f64::from(u8::from(x == 0.0))),
        Node::Binary(op, a, b) => {
            let (a, b) = (ev(a)?, ev(b)?);
            let truth = |c: bool| f64::from(u8::from(c));
            match op {
                Bin::Add => zip(&a, &b, |x, y| x + y)?,
                Bin::Sub => zip(&a, &b, |x, y| x - y)?,
                Bin::Mul => zip(&a, &b, |x, y| x * y)?,
                Bin::Div => zip(&a, &b, |x, y| x / y)?,
                Bin::Rem => zip(&a, &b, |x, y| x.rem_euclid(y))?,
                Bin::Pow => zip(&a, &b, f64::powf)?,
                Bin::Lt => zip(&a, &b, |x, y| truth(x < y))?,
                Bin::Le => zip(&a, &b, |x, y| truth(x <= y))?,
                Bin::Gt => zip(&a, &b, |x, y| truth(x > y))?,
                Bin::Ge => zip(&a, &b, |x, y| truth(x >= y))?,
                Bin::Eq => zip(&a, &b, |x, y| truth(x == y))?,
                Bin::Ne => zip(&a, &b, |x, y| truth(x != y))?,
                Bin::And => zip(&a, &b, |x, y| truth(x != 0.0 && y != 0.0))?,
                Bin::Or => zip(&a, &b, |x, y| truth(x != 0.0 || y != 0.0))?,
            }
        }
        Node::Call(Func::At, args) => {
            let f = ev(&args[1])?.num()?;
            eval(&args[0], lookup, clock, f, elem)?
        }
        Node::Call(f, args) => {
            let a = args.iter().map(ev).collect::<Result<Vec<_>, _>>()?;
            call(*f, a, t).map_err(|e| format!("{f}(): {e}"))?
        }
    })
}

fn call(f: Func, mut a: Vec<Val>, t: f64) -> Result<Val, String> {
    let one = |f: fn(f64) -> f64, a: &[Val]| Ok(a[0].clone().map(f));
    match f {
        Func::Sin => one(f64::sin, &a),
        Func::Cos => one(f64::cos, &a),
        Func::Tan => one(f64::tan, &a),
        Func::Asin => one(f64::asin, &a),
        Func::Acos => one(f64::acos, &a),
        Func::Atan => one(f64::atan, &a),
        Func::Atan2 => zip(&a[0], &a[1], f64::atan2),
        Func::Sqrt => one(f64::sqrt, &a),
        Func::Abs => one(f64::abs, &a),
        Func::Floor => one(f64::floor, &a),
        Func::Ceil => one(f64::ceil, &a),
        Func::Round => one(f64::round, &a),
        Func::Fract => one(|x| x - x.floor(), &a),
        Func::Sign => one(|x| if x == 0.0 { 0.0 } else { x.signum() }, &a),
        // One list: its smallest / largest component; else per component.
        Func::Min | Func::Max => {
            let pick = if f == Func::Min { f64::min } else { f64::max };
            if let [Val::List(l)] = a.as_slice() {
                return l.iter().copied().reduce(pick).map(Val::Num).ok_or_else(|| "an empty list".into());
            }
            let first = a.remove(0);
            a.iter().try_fold(first, |acc, x| zip(&acc, x, pick))
        }
        Func::Clamp => zip(&zip(&a[0], &a[1], f64::max)?, &a[2], f64::min),
        Func::Mix => {
            let d = zip(&a[1], &a[0], |b, a| b - a)?;
            zip(&a[0], &zip(&d, &a[2], |d, k| d * k)?, |a, b| a + b)
        }
        Func::Smoothstep => {
            let span = zip(&a[1], &a[0], |b, a| b - a)?;
            let k = zip(&zip(&a[2], &a[0], |x, a| x - a)?, &span, |x, s| (x / s).clamp(0.0, 1.0))?;
            Ok(k.map(|k| k * k * (3.0 - 2.0 * k)))
        }
        Func::Pow => zip(&a[0], &a[1], f64::powf),
        Func::Exp => one(f64::exp, &a),
        Func::Log => one(f64::ln, &a),
        Func::Rad => one(f64::to_radians, &a),
        Func::Deg => one(f64::to_degrees, &a),
        Func::Length => Ok(Val::Num(match &a[0] {
            Val::Num(x) => x.abs(),
            Val::List(l) => l.iter().map(|x| x * x).sum::<f64>().sqrt(),
        })),
        Func::Dist => call(Func::Length, vec![zip(&a[0], &a[1], |x, y| x - y)?], t),
        // A list condition picks per component.
        Func::If => match &a[0] {
            Val::Num(c) => Ok(a[if *c != 0.0 { 1 } else { 2 }].clone()),
            Val::List(c) => {
                let pick = |v: &Val, i: usize| match v {
                    Val::Num(x) => Ok(*x),
                    Val::List(l) => l.get(i).copied().ok_or_else(|| format!("lists of {} and {} components", c.len(), l.len())),
                };
                let out = c.iter().enumerate().map(|(i, c)| pick(&a[if *c != 0.0 { 1 } else { 2 }], i)).collect::<Result<_, _>>()?;
                Ok(Val::List(out))
            }
        },
        Func::Noise => {
            let seed = a.get(1).map_or(Ok(0.0), Val::num)?;
            Ok(a[0].clone().map(|x| noise(x, seed)))
        }
        Func::Noise3 => {
            let seed = a.get(1).map_or(Ok(0.0), Val::num)?;
            match &a[0] {
                Val::List(v) if v.len() == 3 => Ok(Val::Num(noise3([v[0], v[1], v[2]], seed))),
                v => Err(format!("takes a point [x, y, z], not {}", match v { Val::Num(_) => "a number".to_string(), Val::List(l) => format!("a list of {}", l.len()) })),
            }
        }
        Func::Random => Ok(a[0].clone().map(|s| hash01(s, 0.0))),
        Func::Wiggle => {
            let seed = a.get(2).map_or(Ok(0.0), Val::num)?;
            let n = a[0].clone().map(|freq| noise(t * freq, seed));
            zip(&n, &a[1], |n, amp| n * amp)
        }
        Func::At => unreachable!("at() is evaluated lazily"),
    }
}

/// A repeatable pseudo-random number in 0..1 for (`i`, `seed`).
fn hash01(i: f64, seed: f64) -> f64 {
    let mut h = i.to_bits() ^ seed.to_bits().rotate_left(29) ^ 0x9e37_79b9_7f4a_7c15;
    // splitmix64's finalizer.
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Smooth 1-D gradient noise in -1..1: 0 at whole numbers, a random slope
/// there, eased between them.
fn noise(x: f64, seed: f64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let slope = |k: f64| hash01(k, seed) * 2.0 - 1.0;
    let (a, b) = (slope(i) * f, slope(i + 1.0) * (f - 1.0));
    let s = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    // Gradient noise peaks at ±0.5 in 1-D; scale to about ±1.
    ((a + (b - a) * s) * 2.0).clamp(-1.0, 1.0)
}

/// Smooth 3-D gradient noise in -1..1 (Perlin's, with a hashed gradient
/// at each whole-number corner).
fn noise3(p: [f64; 3], seed: f64) -> f64 {
    let cell = p.map(f64::floor);
    let f = [p[0] - cell[0], p[1] - cell[1], p[2] - cell[2]];
    let ease = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    // A unit gradient at corner `c`, from the hash of its coordinates.
    let grad = |c: [f64; 3]| {
        let h = |k: f64| hash01(c[0] * 73856093.0 + c[1] * 19349663.0 + c[2] * 83492791.0 + k, seed);
        let (z, a) = (h(0.0) * 2.0 - 1.0, h(0.5) * std::f64::consts::TAU);
        let r = (1.0 - z * z).sqrt();
        [r * a.cos(), r * a.sin(), z]
    };
    let mut corners = [0.0; 8];
    for (k, v) in corners.iter_mut().enumerate() {
        let o = [(k & 1) as f64, ((k >> 1) & 1) as f64, ((k >> 2) & 1) as f64];
        let g = grad([cell[0] + o[0], cell[1] + o[1], cell[2] + o[2]]);
        *v = g[0] * (f[0] - o[0]) + g[1] * (f[1] - o[1]) + g[2] * (f[2] - o[2]);
    }
    let (u, v, w) = (ease(f[0]), ease(f[1]), ease(f[2]));
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let x00 = lerp(corners[0], corners[1], u);
    let x10 = lerp(corners[2], corners[3], u);
    let x01 = lerp(corners[4], corners[5], u);
    let x11 = lerp(corners[6], corners[7], u);
    // Gradient noise in 3-D peaks near ±0.87; scale to about ±1.
    (lerp(lerp(x00, x10, v), lerp(x01, x11, v), w) * 1.15).clamp(-1.0, 1.0)
}

/// `src` with references to node `old` pointed at `new` (quoted when
/// `new` isn't a plain name). Untouched if it doesn't parse.
pub fn rename(src: &str, old: &str, new: &str) -> String {
    let Ok(toks) = lex(src) else { return src.to_string() };
    let plain = new.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && new.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let replacement = if plain { new.to_string() } else { format!("\"{new}\"") };
    let mut out = String::new();
    let mut last = 0;
    for (k, (tok, span)) in toks.iter().enumerate() {
        let name = match tok {
            Tok::Ident(s) | Tok::Str(s) => s,
            _ => continue,
        };
        // A node name: followed by '.', not itself after one (a param).
        let is_node = matches!(toks.get(k + 1), Some((Tok::Sym("."), _)))
            && !matches!(k.checked_sub(1).and_then(|j| toks.get(j)), Some((Tok::Sym("."), _)));
        if is_node && name == old {
            out += &src[last..span.start];
            out += &replacement;
            last = span.end;
        }
    }
    out + &src[last..]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A geometry node's formula reads its element by @ names, components
    /// too; outside one, or for a name it hasn't, it says so.
    #[test]
    fn at_names_read_the_element() {
        let elem = |name: &str| match name {
            "P" => Some(Val::List(vec![1.0, 2.0, 3.0])),
            "id" => Some(Val::Num(7.0)),
            _ => None,
        };
        let mut refs = serde_json::Map::new();
        refs.insert("loc.translate".into(), json!([1, 0, 0]));
        let run = |src: &str| parse(src).unwrap().eval_elem(Clock::default(), 1.0, &elem, &refs);
        assert_eq!(run("@P.y * 2 + @id"), Ok(Val::Num(11.0)));
        assert_eq!(run("@P + [1, 1, 1]"), Ok(Val::List(vec![2.0, 3.0, 4.0])));
        assert_eq!(run("random(@id)"), run("random(7)"));
        assert_eq!(run("@P.y > 1 && @id == 7"), Ok(Val::Num(1.0)));
        assert!(run("@Cd").unwrap_err().contains("@Cd"));
        assert!(run("cam.focal + @id").unwrap_err().contains("isn't known"));
        assert_eq!(run("dist(@P, loc.translate)"), Ok(Val::Num(13f64.sqrt())), "a ref resolved beforehand");
        assert!(run("at(loc.translate, 3)").unwrap_err().contains("own frame"));
        // 3-D noise: smooth, -1..1, 0 at whole points, repeatable.
        let n = |p: [f64; 3]| match run(&format!("noise3([{}, {}, {}])", p[0], p[1], p[2])) { Ok(Val::Num(x)) => x, v => panic!("{v:?}") };
        assert_eq!(n([2.0, 3.0, 4.0]), 0.0);
        assert!((n([0.31, 0.5, 0.7]) - n([0.3101, 0.5, 0.7])).abs() < 1e-3 && n([0.31, 0.5, 0.7]) != 0.0);
        assert!(run("noise3(1)").is_err());
        assert!(parse("@P.x * frame").unwrap().uses_time() && !parse("@P.x + loc.translate.x").unwrap().uses_time());
        assert_eq!(parse("@P.x + @id").unwrap().attrs(), vec!["P", "id"]);
        let none: Lookup = &|_, _, _| Err("none".into());
        assert!(parse("@id").unwrap().eval(none, Clock::default(), 1.0).unwrap_err().contains("geometry node"));
        assert!(parse("@ + 1").is_err());
    }

    fn run(src: &str, t: f64) -> Result<Val, String> {
        let lookup = |node: &str, param: &str, t: f64| match (node, param) {
            ("a", "center") => Some(json!([10.0 * t, 20.0])),
            ("a", "size") => Some(json!(4)),
            ("plate-2", "on") => Some(json!(true)),
            ("a", "name") => Some(json!("text")),
            _ => None,
        }
        .ok_or_else(|| format!("no node '{node}'"));
        parse(src)?.eval(&lookup, Clock { fps: 25.0, first_frame: 1001.0 }, t)
    }

    fn num(src: &str) -> f64 {
        run(src, 1.0).unwrap().num().unwrap()
    }

    #[test]
    fn arithmetic_precedence_and_functions() {
        assert_eq!(num("1 + 2 * 3"), 7.0);
        assert_eq!(num("(1 + 2) * 3"), 9.0);
        assert_eq!(num("-2 ^ 2"), -4.0);
        assert_eq!(num("2 ^ 3 ^ 2"), 512.0);
        assert_eq!(num("7 % 3 + -7 % 3"), 3.0);
        assert_eq!(num("1 < 2 && 2 <= 2 || 0"), 1.0);
        assert_eq!(num("!(3 == 3)"), 0.0);
        assert_eq!(num(".5 + 1e1 + 2.5E-1"), 10.75);
        assert!((num("sin(rad(30))") - 0.5).abs() < 1e-12);
        assert_eq!(num("deg(atan2(1, 1))"), 45.0);
        assert_eq!(num("clamp(5, 0, 2) + min(3, 1, 2) + max([1, 9, 4])"), 2.0 + 1.0 + 9.0);
        assert_eq!(num("mix(10, 20, 0.25)"), 12.5);
        assert_eq!(num("smoothstep(0, 10, 5)"), 0.5);
        assert_eq!(num("if(frame > 0, 1, 1 / 0)"), 1.0);
        assert_eq!(run("if([1, 0], [1, 2], 9)", 1.0), Ok(Val::List(vec![1.0, 9.0])));
        assert_eq!(num("length([3, 4]) + dist([1, 1], [4, 5])"), 10.0);
        assert_eq!(num("round(2.5) + floor(-0.5) + fract(1.25) + sign(-3)"), 3.0 - 1.0 + 0.25 - 1.0);
    }

    #[test]
    fn lists_refs_and_time() {
        assert_eq!(run("[1, 2] * 2 + [10, 0]", 1.0), Ok(Val::List(vec![12.0, 4.0])));
        assert_eq!(run("a.center + [5, 0]", 3.0), Ok(Val::List(vec![35.0, 20.0])));
        assert_eq!(num("a.center.y + a.center[0] + a.size"), 20.0 + 10.0 + 4.0);
        assert_eq!(num("\"plate-2\".on"), 1.0);
        assert_eq!(run("at(a.center.x, frame - 2)", 5.0), Ok(Val::Num(30.0)));
        assert_eq!(run("frame * 2", 2.5), Ok(Val::Num(5.0)));
        // 25 fps from frame 1001: frame 1051 is two seconds in.
        assert_eq!(run("time", 1051.0), Ok(Val::Num(2.0)));
        assert_eq!(run("[time, fps]", 1001.0), Ok(Val::List(vec![0.0, 25.0])));
        assert_eq!(run("at(time, frame - fps)", 1051.0), Ok(Val::Num(1.0)), "a second earlier");
        assert_eq!(Clock { fps: 0.0, first_frame: 1.0 }.time(25.0), 1.0, "no rate: 24");
        assert_eq!(parse("a.center.x + \"b c\".size * at(a.size, 1)").unwrap().refs(), [("a", "center"), ("b c", "size"), ("a", "size")]);
        // Errors when run.
        assert_eq!(run("gone.center", 1.0), Err("no node 'gone'".into()));
        assert!(run("a.name", 1.0).unwrap_err().contains("isn't a number"));
        assert!(run("[1, 2] + [1, 2, 3]", 1.0).unwrap_err().contains("2 and 3"));
        assert!(run("a.center[2]", 1.0).unwrap_err().contains("out of range"));
        assert!(run("sin([1, 2]) + atan2([1, 2], 1)", 1.0).is_ok());
    }

    #[test]
    fn parse_errors_say_what_is_wrong() {
        for (src, says) in [
            ("", "empty"),
            ("1 +", "unexpected end"),
            ("(1", "expected ')'"),
            ("foo", "unknown name 'foo'"),
            ("foo(1)", "unknown function 'foo'"),
            ("sin(1, 2)", "sin() takes 1 argument"),
            ("clamp(1)", "takes 3 arguments"),
            ("a.center.q", "unknown component 'q'"),
            ("1 2", "after the formula"),
            ("\"x\"", "needs a param"),
            ("1 $ 2", "unexpected '$'"),
        ] {
            let e = parse(src).unwrap_err();
            assert!(e.contains(says), "{src:?}: {e}");
        }
    }

    #[test]
    fn noise_is_smooth_repeatable_and_bounded() {
        let n = |x: f64| noise(x, 0.0);
        assert_eq!(n(3.0), 0.0);
        assert_eq!(n(3.3), n(3.3));
        assert_ne!(n(3.3), noise(3.3, 1.0), "the seed changes it");
        let mut prev = n(0.0);
        for k in 1..=4000 {
            let v = n(k as f64 * 0.01);
            assert!(v.abs() <= 1.0);
            assert!((v - prev).abs() < 0.05, "smooth at {k}");
            prev = v;
        }
        let r = num("random(7)");
        assert!((0.0..1.0).contains(&r) && r == num("random(7)") && r != num("random(8)"));
        assert_eq!(run("wiggle(0.1, 5, 2)", 37.0), Ok(Val::Num(5.0 * noise(3.7, 2.0))));
    }

    #[test]
    fn renaming_a_node_rewrites_its_references_only() {
        assert_eq!(rename("a.center.x + a.size + b.a + sin(a.x)", "a", "z"), "z.center.x + z.size + b.a + sin(z.x)");
        assert_eq!(rename("a.center", "a", "plate-2"), "\"plate-2\".center");
        assert_eq!(rename("\"plate-2\".center*2", "plate-2", "p"), "p.center*2");
    }
}
