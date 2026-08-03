use clap::Parser;
// use std::process::exit;

#[derive(Parser, Debug)]
pub struct Args {
    /// Input
    input: u128,
}

pub fn run(args: &Args) {
    let fs = factor(args.input);
    println!("{}", prettify(fs))
}

fn factor(n: u128) -> Vec<u128> {
    if n == 0 || n == 1 {
        return vec![];
    }
    let mut n = n;
    let mut div: u128 = 2;
    let mut v: Vec<u128> = vec![];
    while div <= n {
        if !n.is_multiple_of(div) {
            div += 1;
            continue;
        }
        v.push(div);
        n /= div;
    }
    v
}

fn prettify(v: Vec<u128>) -> String {
    if v.is_empty() {
        return "NONE".to_string();
    }
    let mut s = String::new();
    let mut prev: u128 = 0;
    let mut count: u128 = 0;
    for f in v.iter() {
        if prev == *f {
            count += 1;
            continue;
        }
        if prev == 0 {
            prev = *f;
            count = 1;
            continue;
        }
        push_factor(&mut s, prev, count, true);
        prev = *f;
        count = 1;
    }
    if prev == 0 {
        return s;
    }
    push_factor(&mut s, prev, count, false);
    s
}

fn push_factor(s: &mut String, f: u128, pow: u128, put_mult: bool) {
    if pow > 1 {
        s.push_str(&format!("{}{}", f, to_superscript(pow)));
    } else {
        s.push_str(&format!("{}", f));
    }
    if put_mult {
        s.push_str(" × ");
    }
}

fn to_superscript(n: u128) -> String {
    const SUPERSCRIPTS: [&str; 10] = ["⁰", "¹", "²", "³", "⁴", "⁵", "⁶", "⁷", "⁸", "⁹"];
    n.to_string()
        .chars()
        .map(|c| SUPERSCRIPTS[(c as u8 - b'0') as usize])
        .collect()
}
