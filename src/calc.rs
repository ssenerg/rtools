//! A calculator for math written the way it is on paper: powers, roots,
//! logarithms, trigonometric and hyperbolic functions, factorials and more,
//! with exact whole-number results where possible.

use crate::utils;
use clap::Parser;
use std::collections::HashMap;
use std::f64::consts::{E, PI, TAU};
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::exit;

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  rtools calc '2^10 + 5!'                 1144
  rtools calc 'sin(30°) + cos(π/3)'       1
  rtools calc 'log(8, 2) + ln(e^3)'       6
  rtools calc 'x = 3; y = 4; √(x² + y²)'  5
  rtools calc '30!'                       265252859812191058636308480000000
  rtools calc                             type expressions, one per line

Operators: + - × ÷ (or * /), ^ or ² ³ for powers, √ ∛, n! factorial, 50% percent,
30° degrees, |x| absolute value, and `a mod b`. A number or ) right before a name
or ( multiplies: 2π, 3(x+1), 2sin x. It binds tighter than × and ÷, so 1/2π is
1/(2π). Functions work with or without parentheses: sin 30°, sin(30°).

Functions:
  sin cos tan cot sec csc, asin acos atan acot asec acsc (or arcsin …), atan2(y, x)
  sinh cosh tanh coth sech csch, asinh acosh atanh acoth asech acsch
  exp ln log (base 10) log(x, base) log2, sqrt cbrt root(x, n)
  n!, gamma, nCr(n, k), nPr(n, k), abs sign floor ceil round(x, digits) trunc
  mod(a, b) gcd lcm min max hypot, deg(x) (radians to degrees), rad(x)
Constants: pi π, e, tau τ, phi φ. Variables: x = 5, then x; ans is the last result.")]
pub struct Args {
    /// The expression. Several can be separated with ; and variables set with =.
    /// If omitted, reads one expression per line (interactively in a terminal)
    #[arg(value_name = "EXPRESSION", allow_hyphen_values = true)]
    expression: Vec<String>,

    /// Angles in degrees instead of radians
    #[arg(short, long)]
    degrees: bool,

    /// Significant digits to show
    #[arg(short = 'p', long, value_name = "N", default_value_t = 15,
          value_parser = clap::value_parser!(u8).range(1..=17))]
    precision: u8,
}

pub fn run(args: &Args, copy: bool) {
    let options = Options::from_args(args, copy).unwrap_or_else(|e| fail(&e));
    let mut calc = Calculator::new(options.degrees);
    let digits = options.precision;
    let copy = options.copy;

    if !options.expression.is_empty() {
        let input = options.expression.join(" ");
        match calc.run(&input) {
            Ok(Some(result)) => {
                if let Err(e) = utils::emit(&format_number(result, digits), copy) {
                    fail(&e);
                }
            }
            Ok(None) => {}
            Err(e) => fail(&e.show(&input)),
        }
        return;
    }

    let interactive = io::stdin().is_terminal();
    if interactive {
        println!("Type an expression and press Enter. Ctrl-D or \"exit\" quits.");
    }
    let mut failed = false;
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        if interactive {
            print!("> ");
            let _ = io::stdout().flush();
        }
        let Some(Ok(line)) = lines.next() else { break };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if interactive && matches!(line, "exit" | "quit" | "q") {
            break;
        }
        match calc.run(line) {
            Ok(Some(result)) => println!("{}", format_number(result, digits)),
            Ok(None) => {}
            Err(e) => {
                eprintln!("Error: {}", e.show(line));
                failed = true;
            }
        }
    }
    if failed && !interactive {
        exit(1);
    }
}

/// The arguments, with -d, -p and -c picked out from among the expression's
/// parts. Expressions may start with a minus, so everything after the first
/// part reaches us as expression, flags included.
struct Options {
    expression: Vec<String>,
    degrees: bool,
    precision: usize,
    copy: bool,
}

impl Options {
    fn from_args(args: &Args, copy: bool) -> std::result::Result<Options, String> {
        let mut options = Options {
            expression: Vec::new(),
            degrees: args.degrees,
            precision: args.precision as usize,
            copy,
        };
        let mut parts = args.expression.iter();
        while let Some(part) = parts.next() {
            match part.as_str() {
                "-d" | "--degrees" => options.degrees = true,
                "-c" | "--copy" => options.copy = true,
                "-p" | "--precision" => {
                    let value = parts.next().ok_or("--precision needs a number of digits")?;
                    options.precision = precision(value)?;
                }
                flag if flag.starts_with("--precision=") => {
                    options.precision = precision(&flag["--precision=".len()..])?;
                }
                _ => options.expression.push(part.clone()),
            }
        }
        Ok(options)
    }
}

fn precision(text: &str) -> std::result::Result<usize, String> {
    match text.parse::<usize>() {
        Ok(n @ 1..=17) => Ok(n),
        _ => Err(format!("--precision takes 1 to 17 digits, not {text:?}")),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("Error: {message}");
    exit(1);
}

// ---------------------------------------------------------------------------
// Numbers

/// Whole numbers stay exact as long as they fit; anything else is a float.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Num {
    Int(i128),
    Float(f64),
}

use Num::{Float, Int};

impl Num {
    fn value(self) -> f64 {
        match self {
            Int(i) => i as f64,
            Float(f) => f,
        }
    }

    /// The whole number this is, if it is one.
    fn whole(self) -> Option<i128> {
        match self {
            Int(i) => Some(i),
            Float(f) if f.fract() == 0.0 && f.abs() < 1e36 => Some(f as i128),
            Float(_) => None,
        }
    }
}

/// Where in the input something went wrong, as character positions.
#[derive(Debug, PartialEq)]
struct CalcError {
    message: String,
    span: Option<(usize, usize)>,
}

impl CalcError {
    fn new(message: impl Into<String>) -> CalcError {
        CalcError {
            message: message.into(),
            span: None,
        }
    }

    fn at(message: impl Into<String>, start: usize, end: usize) -> CalcError {
        CalcError {
            message: message.into(),
            span: Some((start, end.max(start + 1))),
        }
    }

    /// The message, and the input with the problem underlined.
    fn show(&self, input: &str) -> String {
        match self.span {
            Some((start, end)) => {
                let line: String = input.chars().collect();
                let marks = "^".repeat(end.min(input.chars().count().max(start + 1)) - start);
                format!("{}\n  {line}\n  {}{marks}", self.message, " ".repeat(start))
            }
            None => self.message.clone(),
        }
    }
}

type Result<T> = std::result::Result<T, CalcError>;

fn finite(value: f64, what: &str) -> Result<Num> {
    if value.is_finite() {
        Ok(Float(value))
    } else if value.is_nan() {
        Err(CalcError::new(format!("{what} isn't a real number")))
    } else {
        Err(CalcError::new(format!(
            "{what} is too large (above 1.8e308)"
        )))
    }
}

fn add(a: Num, b: Num) -> Result<Num> {
    match (a, b) {
        (Int(x), Int(y)) => Ok(x.checked_add(y).map_or(Float(x as f64 + y as f64), Int)),
        _ => finite(a.value() + b.value(), "the sum"),
    }
}

fn sub(a: Num, b: Num) -> Result<Num> {
    match (a, b) {
        (Int(x), Int(y)) => Ok(x.checked_sub(y).map_or(Float(x as f64 - y as f64), Int)),
        _ => finite(a.value() - b.value(), "the difference"),
    }
}

fn mul(a: Num, b: Num) -> Result<Num> {
    match (a, b) {
        (Int(x), Int(y)) => Ok(x.checked_mul(y).map_or(Float(x as f64 * y as f64), Int)),
        _ => finite(a.value() * b.value(), "the product"),
    }
}

fn div(a: Num, b: Num) -> Result<Num> {
    if b.value() == 0.0 {
        return Err(CalcError::new("division by zero"));
    }
    match (a, b) {
        (Int(x), Int(y)) if x % y == 0 => Ok(Int(x / y)),
        _ => finite(a.value() / b.value(), "the quotient"),
    }
}

fn neg(a: Num) -> Num {
    match a {
        Int(x) => x.checked_neg().map_or(Float(-(x as f64)), Int),
        Float(f) => Float(-f),
    }
}

fn pow(base: Num, exponent: Num) -> Result<Num> {
    if let (Int(b), Some(e)) = (base, exponent.whole())
        && (0..=u32::MAX as i128).contains(&e)
        && let Some(result) = b.checked_pow(e as u32)
    {
        return Ok(Int(result));
    }
    let (b, e) = (base.value(), exponent.value());
    if b == 0.0 && e < 0.0 {
        return Err(CalcError::new("division by zero: 0 to a negative power"));
    }
    if b < 0.0 && e.fract() != 0.0 {
        return Err(CalcError::new(format!(
            "a negative number to the power {} isn't a real number. For odd roots use root(x, n) or cbrt",
            format_number(exponent, 15)
        )));
    }
    finite(b.powf(e), "the power")
}

fn modulo(a: Num, b: Num) -> Result<Num> {
    if b.value() == 0.0 {
        return Err(CalcError::new("mod by zero"));
    }
    match (a, b) {
        // The result takes the divisor's sign, as in math: -7 mod 3 is 2.
        (Int(x), Int(y)) => {
            let r = x.rem_euclid(y);
            Ok(Int(if y < 0 && r != 0 { r + y } else { r }))
        }
        _ => {
            let (x, y) = (a.value(), b.value());
            finite(x - y * (x / y).floor(), "the remainder")
        }
    }
}

fn factorial(n: Num) -> Result<Num> {
    match n.whole() {
        Some(k) if k < 0 => Err(CalcError::new(
            "the factorial of a negative whole number is undefined",
        )),
        Some(k) if k <= 33 => Ok(Int((1..=k).product())),
        Some(k) if k <= 170 => Ok(Float((1..=k).map(|i| i as f64).product())),
        Some(_) => Err(CalcError::new("the factorial is too large (above 170!)")),
        None => finite(gamma(n.value() + 1.0)?, "the factorial"),
    }
}

/// The gamma function, by the Lanczos approximation, exact for whole numbers.
fn gamma(x: f64) -> Result<f64> {
    if x <= 0.0 && x.fract() == 0.0 {
        return Err(CalcError::new(
            "gamma is undefined at zero and negative whole numbers",
        ));
    }
    if x.fract() == 0.0 && x <= 171.0 {
        return Ok((1..x as i64).map(|i| i as f64).product());
    }
    if x < 0.5 {
        return Ok(PI / ((PI * x).sin() * gamma(1.0 - x)?));
    }
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    let x = x - 1.0;
    let t = x + G + 0.5;
    let sum = C[0] + (1..9).map(|i| C[i] / (x + i as f64)).sum::<f64>();
    Ok((2.0 * PI).sqrt() * t.powf(x + 0.5) * (-t).exp() * sum)
}

fn whole_arg(n: Num, name: &str) -> Result<i128> {
    n.whole()
        .ok_or_else(|| CalcError::new(format!("{name} needs whole numbers")))
}

/// n choose k, exact while it fits.
fn choose(n: Num, k: Num) -> Result<Num> {
    let (n, k) = (whole_arg(n, "nCr")?, whole_arg(k, "nCr")?);
    if n < 0 || k < 0 || k > n {
        return Err(CalcError::new("nCr(n, k) needs 0 ≤ k ≤ n"));
    }
    let k = k.min(n - k);
    let mut exact: Option<i128> = Some(1);
    let mut approx = 1.0f64;
    for i in 1..=k {
        exact = exact.and_then(|r| r.checked_mul(n - k + i)).map(|r| r / i);
        approx = approx * (n - k + i) as f64 / i as f64;
    }
    match exact {
        Some(r) => Ok(Int(r)),
        None => finite(approx, "nCr"),
    }
}

/// Ordered selections of k from n: n! / (n-k)!.
fn permutations(n: Num, k: Num) -> Result<Num> {
    let (n, k) = (whole_arg(n, "nPr")?, whole_arg(k, "nPr")?);
    if n < 0 || k < 0 || k > n {
        return Err(CalcError::new("nPr(n, k) needs 0 ≤ k ≤ n"));
    }
    let mut result = Int(1);
    for i in 0..k {
        result = mul(result, Int(n - i))?;
    }
    Ok(result)
}

fn gcd_of(a: i128, b: i128) -> i128 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// sin and cos, exactly 0 and ±1 at multiples of a right angle, so sin(π) is 0.
/// Not near 0, where sin x ≈ x is the right answer, and not for huge angles,
/// where every float looks like such a multiple.
fn sin_cos(radians: f64) -> (f64, f64) {
    let quarters = radians / (PI / 2.0);
    let nearest = quarters.round();
    if nearest != 0.0
        && nearest.abs() <= 1e6
        && (quarters - nearest).abs() <= 4.0 * f64::EPSILON * nearest.abs()
    {
        return match (nearest as i128).rem_euclid(4) {
            0 => (0.0, 1.0),
            1 => (1.0, 0.0),
            2 => (0.0, -1.0),
            _ => (-1.0, 0.0),
        };
    }
    radians.sin_cos()
}

// ---------------------------------------------------------------------------
// Reading expressions

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(Num),
    Name(String),
    Op(char),
}

/// A token and where it is, in characters.
type Spanned = (Token, usize, usize);

fn tokenize(input: &str) -> Result<Vec<Spanned>> {
    // Persian and Arabic digits, and the Arabic decimal separator.
    let chars: Vec<char> = input
        .chars()
        .map(|c| match c {
            '۰'..='۹' => char::from(b'0' + (c as u32 - '۰' as u32) as u8),
            '٠'..='٩' => char::from(b'0' + (c as u32 - '٠' as u32) as u8),
            '٫' => '.',
            c => c,
        })
        .collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            // 1.5e-3, but not the constant e in 2e.
            if i < chars.len() && matches!(chars[i], 'e' | 'E') {
                let sign = usize::from(matches!(chars.get(i + 1), Some('+' | '-')));
                if chars.get(i + 1 + sign).is_some_and(|d| d.is_ascii_digit()) {
                    i += 1 + sign;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let text: String = chars[start..i].iter().collect();
            let number = if text.chars().all(|c| c.is_ascii_digit()) {
                text.parse::<i128>()
                    .map(Int)
                    .or_else(|_| text.parse::<f64>().map(Float))
            } else {
                text.parse::<f64>().map(Float)
            }
            .map_err(|_| CalcError::at(format!("{text:?} isn't a number"), start, i))?;
            tokens.push((Token::Number(number), start, i));
            continue;
        }
        if c.is_alphabetic() && !"πτφϕ".contains(c) {
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_')
                && !"πτφϕ".contains(chars[i])
                && superscript(chars[i]).is_none()
            {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            tokens.push((Token::Name(name.to_lowercase()), start, i));
            continue;
        }
        // Superscripts: x², x⁻¹.
        if let Some(first) = superscript(c) {
            let mut text = String::from(first);
            i += 1;
            while let Some(d) = chars.get(i).and_then(|&c| superscript(c)) {
                text.push(d);
                i += 1;
            }
            let exponent = text
                .parse::<i128>()
                .map_err(|_| CalcError::at("that superscript isn't a number", start, i))?;
            tokens.push((Token::Op('^'), start, i));
            tokens.push((Token::Number(Int(exponent)), start, i));
            continue;
        }
        let token = match c {
            'π' => Token::Name("pi".into()),
            'τ' => Token::Name("tau".into()),
            'φ' | 'ϕ' => Token::Name("phi".into()),
            '*' if chars.get(i + 1) == Some(&'*') => {
                i += 1;
                Token::Op('^')
            }
            '*' | '×' | '·' | '⋅' | '∗' => Token::Op('*'),
            '/' | '÷' | '∕' => Token::Op('/'),
            '-' | '−' | '–' => Token::Op('-'),
            '[' => Token::Op('('),
            ']' => Token::Op(')'),
            '+' | '^' | '(' | ')' | ',' | '!' | '%' | '°' | '|' | '√' | '∛' | '∜' | '=' | ';' => {
                Token::Op(c)
            }
            _ => {
                return Err(CalcError::at(
                    format!("{c:?} isn't something a calculation can use"),
                    start,
                    start + 1,
                ));
            }
        };
        i += 1;
        tokens.push((token, start, i));
    }
    Ok(tokens)
}

fn superscript(c: char) -> Option<char> {
    Some(match c {
        '⁰' => '0',
        '¹' => '1',
        '²' => '2',
        '³' => '3',
        '⁴' => '4',
        '⁵' => '5',
        '⁶' => '6',
        '⁷' => '7',
        '⁸' => '8',
        '⁹' => '9',
        '⁻' => '-',
        _ => return None,
    })
}

const CONSTANTS: [(&str, f64); 5] = [
    ("pi", PI),
    ("e", E),
    ("tau", TAU),
    ("phi", 1.618_033_988_749_895),
    ("golden", 1.618_033_988_749_895),
];

/// Every function's names, and how many arguments it takes.
const FUNCTIONS: &[(&[&str], usize, usize)] = &[
    (&["sin"], 1, 1),
    (&["cos"], 1, 1),
    (&["tan", "tg"], 1, 1),
    (&["cot", "ctg", "cotg", "cotan"], 1, 1),
    (&["sec"], 1, 1),
    (&["csc", "cosec"], 1, 1),
    (&["asin", "arcsin"], 1, 1),
    (&["acos", "arccos"], 1, 1),
    (&["atan", "arctan", "arctg"], 1, 1),
    (&["acot", "arccot", "arcctg"], 1, 1),
    (&["asec", "arcsec"], 1, 1),
    (&["acsc", "arccsc", "arccosec"], 1, 1),
    (&["atan2"], 2, 2),
    (&["sinh"], 1, 1),
    (&["cosh"], 1, 1),
    (&["tanh"], 1, 1),
    (&["coth"], 1, 1),
    (&["sech"], 1, 1),
    (&["csch", "cosech"], 1, 1),
    (&["asinh", "arsinh", "arcsinh"], 1, 1),
    (&["acosh", "arcosh", "arccosh"], 1, 1),
    (&["atanh", "artanh", "arctanh"], 1, 1),
    (&["acoth", "arcoth", "arccoth"], 1, 1),
    (&["asech", "arsech", "arcsech"], 1, 1),
    (&["acsch", "arcsch", "arccsch"], 1, 1),
    (&["exp"], 1, 1),
    (&["ln"], 1, 1),
    (&["log"], 1, 2),
    (&["log10", "lg"], 1, 1),
    (&["log2", "lb"], 1, 1),
    (&["sqrt"], 1, 1),
    (&["cbrt"], 1, 1),
    (&["root"], 2, 2),
    (&["abs"], 1, 1),
    (&["sign", "sgn"], 1, 1),
    (&["floor"], 1, 1),
    (&["ceil"], 1, 1),
    (&["round"], 1, 2),
    (&["trunc"], 1, 1),
    (&["frac"], 1, 1),
    (&["gamma"], 1, 1),
    (&["fact", "factorial"], 1, 1),
    (&["ncr", "binom"], 2, 2),
    (&["npr"], 2, 2),
    (&["mod"], 2, 2),
    (&["gcd"], 2, usize::MAX),
    (&["lcm"], 2, usize::MAX),
    (&["min"], 1, usize::MAX),
    (&["max"], 1, usize::MAX),
    (&["hypot"], 2, 2),
    (&["deg"], 1, 1),
    (&["rad"], 1, 1),
];

fn function(name: &str) -> Option<(&'static str, usize, usize)> {
    FUNCTIONS
        .iter()
        .find(|(names, ..)| names.contains(&name))
        .map(|(names, min, max)| (names[0], *min, *max))
}

struct Calculator {
    degrees: bool,
    variables: HashMap<String, Num>,
}

impl Calculator {
    fn new(degrees: bool) -> Calculator {
        Calculator {
            degrees,
            variables: HashMap::new(),
        }
    }

    /// Runs `;`-separated statements, returning the last expression's value.
    fn run(&mut self, input: &str) -> Result<Option<Num>> {
        let tokens = tokenize(input)?;
        let mut last = None;
        for statement in tokens.split(|(t, ..)| *t == Token::Op(';')) {
            if statement.is_empty() {
                continue;
            }
            // `x = …` sets a variable.
            if let [
                (Token::Name(name), start, end),
                (Token::Op('='), ..),
                rest @ ..,
            ] = statement
            {
                if function(name).is_some() || CONSTANTS.iter().any(|(c, _)| c == name) {
                    return Err(CalcError::at(
                        format!("{name} is a built-in name, so it can't be a variable"),
                        *start,
                        *end,
                    ));
                }
                let value = self.evaluate(rest, input)?;
                self.variables.insert(name.clone(), value);
                continue;
            }
            let value = self.evaluate(statement, input)?;
            self.variables.insert("ans".to_string(), value);
            last = Some(value);
        }
        Ok(last)
    }

    fn evaluate(&self, tokens: &[Spanned], input: &str) -> Result<Num> {
        if tokens.is_empty() {
            return Err(CalcError::new("there's nothing to calculate"));
        }
        let mut parser = Reader {
            calc: self,
            tokens,
            at: 0,
            abs_depth: 0,
            end: input.chars().count(),
        };
        let value = parser.expression()?;
        if let Some((token, start, end)) = parser.peek_spanned() {
            let message = match token {
                Token::Op(')') => "there's a ) without a matching (".to_string(),
                Token::Number(_) => {
                    "a number follows another without an operator between them".to_string()
                }
                _ => "this doesn't fit here".to_string(),
            };
            return Err(CalcError::at(message, start, end));
        }
        Ok(value)
    }

    fn angle_in(&self, x: f64) -> f64 {
        if self.degrees { x.to_radians() } else { x }
    }

    fn angle_out(&self, radians: f64) -> Result<Num> {
        finite(
            if self.degrees {
                radians.to_degrees()
            } else {
                radians
            },
            "the angle",
        )
    }

    fn call(&self, name: &str, args: &[Num]) -> Result<Num> {
        let x = args[0].value();
        let (sin, cos) = sin_cos(self.angle_in(x));
        let undefined = |what: &str| Err(CalcError::new(format!("{name} is undefined {what}")));
        let domain = |what: &str| Err(CalcError::new(format!("{name} needs {what}")));
        let f = |v: f64| finite(v, &format!("{name}(…)"));
        match name {
            "sin" => f(sin),
            "cos" => f(cos),
            "tan" if cos == 0.0 => undefined("where cos is 0"),
            "tan" => f(sin / cos),
            "cot" if sin == 0.0 => undefined("where sin is 0"),
            "cot" => f(cos / sin),
            "sec" if cos == 0.0 => undefined("where cos is 0"),
            "sec" => f(1.0 / cos),
            "csc" if sin == 0.0 => undefined("where sin is 0"),
            "csc" => f(1.0 / sin),
            "asin" | "acos" if x.abs() > 1.0 => domain("a number from -1 to 1"),
            "asin" => self.angle_out(x.asin()),
            "acos" => self.angle_out(x.acos()),
            "atan" => self.angle_out(x.atan()),
            "acot" => self.angle_out(PI / 2.0 - x.atan()),
            "asec" | "acsc" if x.abs() < 1.0 => domain("a number that isn't between -1 and 1"),
            "asec" => self.angle_out((1.0 / x).acos()),
            "acsc" => self.angle_out((1.0 / x).asin()),
            "atan2" if x == 0.0 && args[1].value() == 0.0 => undefined("at (0, 0)"),
            "atan2" => self.angle_out(x.atan2(args[1].value())),
            "sinh" => f(x.sinh()),
            "cosh" => f(x.cosh()),
            "tanh" => f(x.tanh()),
            "coth" | "csch" if x == 0.0 => undefined("at 0"),
            "coth" => f(1.0 / x.tanh()),
            "sech" => f(1.0 / x.cosh()),
            "csch" => f(1.0 / x.sinh()),
            "asinh" => f(x.asinh()),
            "acosh" if x < 1.0 => domain("a number of at least 1"),
            "acosh" => f(x.acosh()),
            "atanh" if x.abs() >= 1.0 => domain("a number between -1 and 1"),
            "atanh" => f(x.atanh()),
            "acoth" if x.abs() <= 1.0 => domain("a number that isn't from -1 to 1"),
            "acoth" => f((1.0 / x).atanh()),
            "asech" if x <= 0.0 || x > 1.0 => domain("a number above 0, up to 1"),
            "asech" => f((1.0 / x).acosh()),
            "acsch" if x == 0.0 => undefined("at 0"),
            "acsch" => f((1.0 / x).asinh()),
            "exp" => f(x.exp()),
            "ln" | "log10" | "log2" if x <= 0.0 => domain("a number above 0"),
            "ln" => f(x.ln()),
            "log10" => f(x.log10()),
            "log2" => f(x.log2()),
            "log" => {
                if x <= 0.0 {
                    return domain("a number above 0");
                }
                match args.get(1).map(|b| b.value()) {
                    None => f(x.log10()),
                    Some(b) if b <= 0.0 || b == 1.0 => domain("a base above 0 that isn't 1"),
                    Some(b) => f(x.ln() / b.ln()),
                }
            }
            "sqrt" => {
                if x < 0.0 {
                    return domain("a number that isn't negative");
                }
                // Perfect squares stay exact: √144 is 12.
                let root = x.sqrt().round();
                if let Int(i) = args[0]
                    && (root as i128).checked_mul(root as i128) == Some(i)
                {
                    return Ok(Int(root as i128));
                }
                f(x.sqrt())
            }
            "cbrt" => f(x.cbrt()),
            "root" => {
                let n = whole_arg(args[1], "root")?;
                if n == 0 {
                    return domain("a degree that isn't 0");
                }
                if x < 0.0 && n % 2 == 0 {
                    return domain("a number that isn't negative for an even root");
                }
                let r = x.abs().powf(1.0 / n as f64).copysign(x);
                // Whole roots of whole numbers come out whole: root(27, 3) is 3.
                let rounded = r.round();
                if args[0].whole().is_some()
                    && rounded != 0.0
                    && pow(Float(rounded), Int(n))?.value() == x
                {
                    return Ok(Int(rounded as i128));
                }
                f(r)
            }
            "abs" => Ok(match args[0] {
                Int(i) => Int(i.abs()),
                Float(v) => Float(v.abs()),
            }),
            "sign" => Ok(Int(if x > 0.0 {
                1
            } else if x < 0.0 {
                -1
            } else {
                0
            })),
            "floor" | "ceil" | "trunc" => {
                if let Int(i) = args[0] {
                    return Ok(Int(i));
                }
                let v = match name {
                    "floor" => x.floor(),
                    "ceil" => x.ceil(),
                    _ => x.trunc(),
                };
                Ok(whole_or_float(v))
            }
            "round" => match args.get(1) {
                None => Ok(match args[0] {
                    Int(i) => Int(i),
                    Float(v) => whole_or_float(v.round()),
                }),
                Some(digits) => {
                    let d = whole_arg(*digits, "round")?;
                    let scale = 10f64.powi(d.clamp(-300, 300) as i32);
                    f((x * scale).round() / scale)
                }
            },
            "frac" => f(x.fract()),
            "gamma" => f(gamma(x)?),
            "fact" => factorial(args[0]),
            "ncr" => choose(args[0], args[1]),
            "npr" => permutations(args[0], args[1]),
            "mod" => modulo(args[0], args[1]),
            "gcd" | "lcm" => {
                let mut result = whole_arg(args[0], name)?.abs();
                for arg in &args[1..] {
                    let b = whole_arg(*arg, name)?.abs();
                    result = if name == "gcd" {
                        gcd_of(result, b)
                    } else if result == 0 || b == 0 {
                        0
                    } else {
                        let g = gcd_of(result, b);
                        match (result / g).checked_mul(b) {
                            Some(r) => r,
                            None => return f(result as f64 / g as f64 * b as f64),
                        }
                    };
                }
                Ok(Int(result))
            }
            "min" => Ok(*args
                .iter()
                .min_by(|a, b| a.value().total_cmp(&b.value()))
                .expect("at least one argument")),
            "max" => Ok(*args
                .iter()
                .max_by(|a, b| a.value().total_cmp(&b.value()))
                .expect("at least one argument")),
            "hypot" => f(x.hypot(args[1].value())),
            "deg" => f(x.to_degrees()),
            "rad" => f(x.to_radians()),
            _ => unreachable!("every function in FUNCTIONS is handled"),
        }
    }
}

fn whole_or_float(v: f64) -> Num {
    if v.abs() < 1e36 {
        Int(v as i128)
    } else {
        Float(v)
    }
}

/// Precedence, lowest first: + −, × ÷ mod, unary −, implicit multiplication
/// (2π), ^ (right to left), postfix ! % ° ², then numbers, names and brackets.
struct Reader<'a> {
    calc: &'a Calculator,
    tokens: &'a [Spanned],
    at: usize,
    /// How many |…| are open, so a | knows whether it closes one.
    abs_depth: usize,
    /// Where the input ends, for errors about what's missing there.
    end: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at).map(|(t, ..)| t)
    }

    fn peek_spanned(&self) -> Option<(Token, usize, usize)> {
        self.tokens.get(self.at).cloned()
    }

    fn position(&self) -> (usize, usize) {
        self.tokens
            .get(self.at)
            .map_or((self.end, self.end + 1), |(_, s, e)| (*s, *e))
    }

    fn eat(&mut self, op: char) -> bool {
        if self.peek() == Some(&Token::Op(op)) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// Attaches the current position to errors that don't have one.
    fn here<T>(&self, result: Result<T>, start: usize) -> Result<T> {
        result.map_err(|e| match e.span {
            Some(_) => e,
            None => {
                let end = self
                    .tokens
                    .get(self.at.saturating_sub(1))
                    .map_or(start + 1, |(_, _, e)| *e);
                CalcError::at(e.message, start, end)
            }
        })
    }

    fn start(&self) -> usize {
        self.position().0
    }

    fn expression(&mut self) -> Result<Num> {
        let start = self.start();
        let mut value = self.product()?;
        loop {
            if self.eat('+') {
                let rhs = self.product()?;
                value = self.here(add(value, rhs), start)?;
            } else if self.eat('-') {
                let rhs = self.product()?;
                value = self.here(sub(value, rhs), start)?;
            } else {
                return Ok(value);
            }
        }
    }

    fn product(&mut self) -> Result<Num> {
        let start = self.start();
        let mut value = self.unary()?;
        loop {
            if self.eat('*') {
                let rhs = self.unary()?;
                value = self.here(mul(value, rhs), start)?;
            } else if self.eat('/') {
                let rhs = self.unary()?;
                value = self.here(div(value, rhs), start)?;
            } else if self.peek() == Some(&Token::Name("mod".into())) {
                self.at += 1;
                let rhs = self.unary()?;
                value = self.here(modulo(value, rhs), start)?;
            } else {
                return Ok(value);
            }
        }
    }

    fn unary(&mut self) -> Result<Num> {
        if self.eat('-') {
            return Ok(neg(self.unary()?));
        }
        if self.eat('+') {
            return self.unary();
        }
        self.implicit(false)
    }

    /// 2π, 3(x+1), 2 sin x: terms side by side multiply. As a function's
    /// argument without brackets it stops at the next function, so
    /// sin x cos x is sin(x)·cos(x).
    fn implicit(&mut self, argument: bool) -> Result<Num> {
        let start = self.start();
        let mut value = self.power()?;
        loop {
            let next = match self.peek() {
                Some(Token::Name(name)) if name == "mod" => break,
                Some(Token::Name(name)) if argument && function(name).is_some() => break,
                Some(Token::Name(_)) => true,
                Some(Token::Op('(' | '√' | '∛' | '∜')) => true,
                Some(Token::Op('|')) => self.abs_depth == 0,
                _ => false,
            };
            if !next {
                break;
            }
            let rhs = self.power()?;
            value = self.here(mul(value, rhs), start)?;
        }
        Ok(value)
    }

    fn power(&mut self) -> Result<Num> {
        let start = self.start();
        let base = self.postfix()?;
        if self.eat('^') {
            // Right to left, and the exponent may be negative: 2^-1, 2^3^2.
            let exponent = if self.eat('-') {
                neg(self.power()?)
            } else {
                self.eat('+');
                self.power()?
            };
            return self.here(pow(base, exponent), start);
        }
        Ok(base)
    }

    fn postfix(&mut self) -> Result<Num> {
        let start = self.start();
        let mut value = self.primary()?;
        loop {
            if self.eat('!') {
                value = self.here(factorial(value), start)?;
            } else if self.eat('%') {
                value = self.here(div(value, Int(100)), start)?;
            } else if self.eat('°') {
                // Degrees: radians when angles are in radians, as is otherwise.
                if !self.calc.degrees {
                    value = Float(value.value().to_radians());
                }
            } else {
                return Ok(value);
            }
        }
    }

    fn primary(&mut self) -> Result<Num> {
        let Some((token, start, end)) = self.peek_spanned() else {
            return Err(CalcError::at(
                "the expression ends too early",
                self.end,
                self.end + 1,
            ));
        };
        self.at += 1;
        match token {
            Token::Number(n) => Ok(n),
            Token::Op('(') => {
                let depth = self.abs_depth;
                self.abs_depth = 0;
                let value = self.expression()?;
                self.abs_depth = depth;
                if !self.eat(')') {
                    let (s, e) = self.position();
                    return Err(CalcError::at("a ( is missing its )", s.min(self.end), e));
                }
                Ok(value)
            }
            Token::Op('|') => {
                self.abs_depth += 1;
                let value = self.expression()?;
                self.abs_depth -= 1;
                if !self.eat('|') {
                    let (s, e) = self.position();
                    return Err(CalcError::at(
                        "a | is missing its closing |",
                        s.min(self.end),
                        e,
                    ));
                }
                self.here(self.calc.call("abs", &[value]), start)
            }
            Token::Op(root @ ('√' | '∛' | '∜')) => {
                let value = if self.eat('-') {
                    neg(self.power()?)
                } else {
                    self.power()?
                };
                let name = match root {
                    '√' => "sqrt",
                    _ => "root",
                };
                let degree = Int(if root == '∛' { 3 } else { 4 });
                let args = if name == "sqrt" {
                    vec![value]
                } else {
                    vec![value, degree]
                };
                self.here(self.calc.call(name, &args), start)
            }
            Token::Name(name) => self.name(&name, start, end),
            Token::Op(op) => Err(CalcError::at(
                match op {
                    ')' => "there's a ) without a matching (".to_string(),
                    '=' => "= only sets a variable, like x = 5".to_string(),
                    _ => format!("{op} needs a number before it"),
                },
                start,
                end,
            )),
        }
    }

    fn name(&mut self, name: &str, start: usize, end: usize) -> Result<Num> {
        if let Some((canonical, min, max)) = function(name) {
            // sin²x and sin^2 x mean (sin x)².
            let mut exponent = None;
            if self.eat('^') {
                if self.peek() == Some(&Token::Op('-')) {
                    return Err(CalcError::at(
                        format!(
                            "write a{canonical} for the inverse, or ({canonical} x)^-1 for 1/{canonical} x"
                        ),
                        start,
                        end,
                    ));
                }
                exponent = Some(self.primary()?);
            }
            let args = if self.eat('(') {
                let mut args = vec![self.expression()?];
                while self.eat(',') {
                    args.push(self.expression()?);
                }
                if !self.eat(')') {
                    let (s, e) = self.position();
                    return Err(CalcError::at(
                        format!("{name}( is missing its )"),
                        s.min(self.end),
                        e,
                    ));
                }
                args
            } else {
                // Without brackets: sin 30°, sin 2x, ln x².
                if self.peek().is_none() {
                    return Err(CalcError::at(
                        format!("{name} needs a value, like {name}(1)"),
                        start,
                        end,
                    ));
                }
                vec![if self.eat('-') {
                    neg(self.implicit(true)?)
                } else {
                    self.implicit(true)?
                }]
            };
            if args.len() < min || args.len() > max {
                let wanted = match (min, max) {
                    (1, 1) => "one value".to_string(),
                    (2, 2) => "two values".to_string(),
                    (a, b) if b == usize::MAX => format!("at least {a} values"),
                    (a, b) => format!("{a} or {b} values"),
                };
                return Err(CalcError::at(format!("{name} takes {wanted}"), start, end));
            }
            let value = self.here(self.calc.call(canonical, &args), start)?;
            return match exponent {
                Some(e) => self.here(pow(value, e), start),
                None => Ok(value),
            };
        }
        if let Some((_, value)) = CONSTANTS.iter().find(|(c, _)| *c == name) {
            return Ok(Float(*value));
        }
        if let Some(value) = self.calc.variables.get(name) {
            return Ok(*value);
        }
        Err(CalcError::at(
            format!(
                "unknown name {name:?}. Set it first with {name} = …, or write products with ×, like x × y"
            ),
            start,
            end,
        ))
    }
}

// ---------------------------------------------------------------------------
// Showing numbers

/// Whole numbers in full; others to `digits` significant digits, with
/// scientific notation for very large and very small ones.
fn format_number(n: Num, digits: usize) -> String {
    let x = match n {
        Int(i) => return i.to_string(),
        Float(x) => x,
    };
    if x == 0.0 {
        return "0".to_string();
    }
    let scientific = format!("{:.*e}", digits.saturating_sub(1), x);
    let (mantissa, exponent) = scientific.split_once('e').expect("{:e} has an e");
    let exponent: i32 = exponent.parse().expect("a whole exponent");
    let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
    if (-7..21).contains(&exponent) {
        // Back to plain notation from the rounded digits.
        let negative = mantissa.starts_with('-');
        let digits_only: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
        let point = exponent + 1;
        let text = if point <= 0 {
            format!("0.{}{digits_only}", "0".repeat((-point) as usize))
        } else if point as usize >= digits_only.len() {
            format!(
                "{digits_only}{}",
                "0".repeat(point as usize - digits_only.len())
            )
        } else {
            format!(
                "{}.{}",
                &digits_only[..point as usize],
                &digits_only[point as usize..]
            )
        };
        format!("{}{text}", if negative { "-" } else { "" })
    } else {
        format!("{mantissa}e{exponent}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(input: &str) -> String {
        calc_with(input, false)
    }

    fn calc_with(input: &str, degrees: bool) -> String {
        match Calculator::new(degrees).run(input) {
            Ok(Some(n)) => format_number(n, 15),
            Ok(None) => String::new(),
            Err(e) => format!("error: {}", e.message),
        }
    }

    #[test]
    fn follows_the_order_of_operations() {
        let cases = [
            ("1 + 2 * 3", "7"),
            ("(1 + 2) * 3", "9"),
            ("2^3^2", "512"),
            ("-2^2", "-4"),
            ("2^-1", "0.5"),
            ("-3!", "-6"),
            ("10 - 4 - 3", "3"),
            ("100 / 10 / 5", "2"),
            ("2 × 3 ÷ 4 − 1", "0.5"),
            ("2 ** 3", "8"),
            ("[1 + 2] * 2", "6"),
        ];
        for (input, want) in cases {
            assert_eq!(calc(input), want, "{input}");
        }
    }

    #[test]
    fn multiplies_what_is_written_side_by_side() {
        let cases = [
            ("2π", "6.28318530717959"),
            ("3(4 + 1)", "15"),
            ("(1 + 2)(3 + 4)", "21"),
            // It binds tighter than ÷, as on scientific calculators.
            ("1/2π", "0.159154943091895"),
            ("2^3π", "25.1327412287183"),
            ("x = 5; 2x² - 3x + 1", "36"),
            ("x = 1; 2 sin x cos x", "0.909297426825682"),
            ("2|-3|", "6"),
            ("2e", "5.43656365691809"),
        ];
        for (input, want) in cases {
            assert_eq!(calc(input), want, "{input}");
        }
        assert!(calc("5 5").contains("without an operator"));
    }

    #[test]
    fn keeps_whole_numbers_exact() {
        assert_eq!(calc("30!"), "265252859812191058636308480000000");
        assert_eq!(calc("2^100"), "1267650600228229401496703205376");
        assert_eq!(calc("nCr(52, 5)"), "2598960");
        assert_eq!(calc("nPr(10, 3)"), "720");
        assert_eq!(calc("√144"), "12");
        assert_eq!(calc("root(32, 5)"), "2");
        assert_eq!(calc("∛-27"), "-3");
        assert_eq!(calc("gcd(12, 18, 30)"), "6");
        assert_eq!(calc("lcm(4, 6, 10)"), "60");
        assert_eq!(calc("12 / 4"), "3");
        assert_eq!(calc("7 / 2"), "3.5");
        assert_eq!(calc("2^1000"), "1.07150860718627e301");
        assert_eq!(calc("170!"), "7.25741561530799e306");
    }

    #[test]
    fn has_trigonometric_functions_exact_at_right_angles() {
        let cases = [
            ("sin(pi)", "0"),
            ("cos(pi/2)", "0"),
            ("sin(30°) + cos(π/3)", "1"),
            ("tan(45°)", "1"),
            ("sin²(30°) + cos²(30°)", "1"),
            ("cot(45°)", "1"),
            ("sec(60°)", "2"),
            ("csc(30°)", "2"),
            ("asin(1)", "1.5707963267949"),
            ("acos(-1)", "3.14159265358979"),
            ("atan2(1, 1)", "0.785398163397448"),
            ("acot(1)", "0.785398163397448"),
            ("asec(2)", "1.0471975511966"),
            ("acsc(2)", "0.523598775598299"),
            ("sin(1e-100)", "1e-100"),
        ];
        for (input, want) in cases {
            assert_eq!(calc(input), want, "{input}");
        }
        assert!(calc("tan(90°)").contains("undefined"));
        assert!(calc("cot(0)").contains("undefined"));
        assert!(calc("asin(2)").contains("-1 to 1"));
        // Huge angles use the real sine, not the right-angle shortcut.
        assert_eq!(calc("sin(1e22)"), "-0.852200849767189");
    }

    #[test]
    fn works_in_degrees() {
        assert_eq!(calc_with("sin 30", true), "0.5");
        assert_eq!(calc_with("sin(30°)", true), "0.5");
        assert_eq!(calc_with("asin(0.5)", true), "30");
        assert_eq!(calc_with("atan 1", true), "45");
        assert_eq!(calc("deg(pi)"), "180");
        assert_eq!(calc("rad(180)"), "3.14159265358979");
    }

    #[test]
    fn has_hyperbolic_functions() {
        let cases = [
            ("sinh 1", "1.1752011936438"),
            ("cosh 0", "1"),
            ("tanh 1", "0.761594155955765"),
            ("coth 1", "1.31303528549933"),
            ("sech 0", "1"),
            ("csch 1", "0.850918128239322"),
            ("asinh 1", "0.881373587019543"),
            ("acosh 2", "1.31695789692482"),
            ("atanh 0.5", "0.549306144334055"),
            ("acoth 2", "0.549306144334055"),
            ("asech 0.5", "1.31695789692482"),
            ("acsch 1", "0.881373587019543"),
        ];
        for (input, want) in cases {
            assert_eq!(calc(input), want, "{input}");
        }
        assert!(calc("coth 0").contains("undefined"));
        assert!(calc("acosh 0.5").contains("at least 1"));
        assert!(calc("atanh 1").contains("between -1 and 1"));
    }

    #[test]
    fn has_logarithms_roots_and_more() {
        let cases = [
            ("log(8, 2) + ln(e^3)", "6"),
            ("log 1000", "3"),
            ("log2 1024", "10"),
            ("exp(1)", "2.71828182845905"),
            ("sqrt 2", "1.4142135623731"),
            ("0.5!", "0.886226925452758"),
            ("gamma(0.5)^2", "3.14159265358979"),
            ("abs -3", "3"),
            ("floor(-2.5)", "-3"),
            ("ceil(2.1)", "3"),
            ("round(2.5)", "3"),
            ("round(pi, 3)", "3.142"),
            ("sign(-4)", "-1"),
            ("7 mod 3", "1"),
            ("-7 mod 3", "2"),
            ("mod(7, -3)", "-2"),
            ("max(3, 9, 1)", "9"),
            ("hypot(3, 4)", "5"),
            ("50% * 80", "40"),
            ("0.1 + 0.2", "0.3"),
            ("۲ + ۳", "5"),
            ("1e-3 * 2e3", "2"),
        ];
        for (input, want) in cases {
            assert_eq!(calc(input), want, "{input}");
        }
    }

    #[test]
    fn explains_what_is_wrong() {
        let cases = [
            ("1/0", "division by zero"),
            ("sqrt(-1)", "isn't negative"),
            ("ln 0", "above 0"),
            ("(-8)^(1/3)", "isn't a real number"),
            ("log(8, 1)", "isn't 1"),
            ("1000!", "too large"),
            ("exp(710)", "too large"),
            ("(2 + 3", "missing its )"),
            ("2 + 3)", "without a matching ("),
            ("2 +", "ends too early"),
            ("sine(3)", "unknown name \"sine\""),
            ("sin", "needs a value"),
            ("hypot(3)", "two values"),
            ("sin^-1(0.5)", "write asin"),
            ("pi = 3", "built-in name"),
            ("(-1)!", "negative whole number"),
            ("2 # 3", "isn't something"),
        ];
        for (input, want) in cases {
            let got = calc(input);
            assert!(got.contains(want), "{input}: {got}");
        }
        let error = Calculator::new(false).run("2 + sine(3)").unwrap_err();
        assert_eq!(
            error.show("2 + sine(3)"),
            "unknown name \"sine\". Set it first with sine = …, or write products with ×, like x × y\n  2 + sine(3)\n      ^^^^"
        );
    }

    #[test]
    fn remembers_variables_and_the_last_answer() {
        let mut calc = Calculator::new(false);
        assert_eq!(calc.run("r = 2"), Ok(None));
        assert_eq!(
            calc.run("π r²").map(|n| n.map(|n| format_number(n, 6))),
            Ok(Some("12.5664".to_string()))
        );
        assert_eq!(
            calc.run("ans / 4").map(|n| n.map(|n| format_number(n, 6))),
            Ok(Some("3.14159".to_string()))
        );
    }

    #[test]
    fn shows_numbers_plainly() {
        assert_eq!(format_number(Float(1024.5), 15), "1024.5");
        assert_eq!(format_number(Float(-0.000125), 15), "-0.000125");
        assert_eq!(format_number(Float(1e21), 15), "1e21");
        assert_eq!(format_number(Float(1e-8), 15), "1e-8");
        assert_eq!(format_number(Float(2.0 / 3.0), 5), "0.66667");
        assert_eq!(format_number(Float(-0.0), 15), "0");
        assert_eq!(format_number(Int(-42), 15), "-42");
    }

    #[test]
    fn picks_flags_out_of_the_expression() {
        let args = Args::parse_from(["calc", "sin(30)", "-d", "-p", "5", "+", "1"]);
        let options = Options::from_args(&args, false).unwrap();
        assert!(options.degrees);
        assert_eq!(options.precision, 5);
        assert_eq!(options.expression, ["sin(30)", "+", "1"]);
        let args = Args::parse_from(["calc", "-5", "+", "3"]);
        assert_eq!(
            Options::from_args(&args, false).unwrap().expression,
            ["-5", "+", "3"]
        );
    }
}
