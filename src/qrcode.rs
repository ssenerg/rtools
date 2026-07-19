use crate::utils;
use clap::Parser;

#[derive(Parser, Debug)]
pub struct Args {}

pub fn run(_args: &Args, copy: bool) {
    utils::no_copy_support(copy);
}
