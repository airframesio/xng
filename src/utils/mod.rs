use lazy_static::lazy_static;
use regex::Regex;

pub mod timestamp;

pub fn normalize_tail(tail: String) -> String {
    lazy_static! {
        static ref TAIL_NORM_RE: Regex = Regex::new(r"[\.\- ]").unwrap();
    }

    TAIL_NORM_RE.replace_all(&tail, "").to_string()
}
