use lazy_static::lazy_static;
use regex::Regex;

pub mod timestamp;

pub fn normalize_tail(tail: &String) -> String {
    lazy_static! {
        static ref TAIL_NORM_RE: Regex = Regex::new(r"[\.\- ]").unwrap();
    }

    TAIL_NORM_RE.replace_all(&tail, "").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_tail_no_special_chars() {
        let tail = String::from("N12345");
        assert_eq!(normalize_tail(&tail), "N12345");
    }

    #[test]
    fn test_normalize_tail_with_hyphens() {
        let tail = String::from("N-12-345");
        assert_eq!(normalize_tail(&tail), "N12345");
    }

    #[test]
    fn test_normalize_tail_with_dots() {
        let tail = String::from("N.12.345");
        assert_eq!(normalize_tail(&tail), "N12345");
    }

    #[test]
    fn test_normalize_tail_with_spaces() {
        let tail = String::from("N 12 345");
        assert_eq!(normalize_tail(&tail), "N12345");
    }

    #[test]
    fn test_normalize_tail_mixed_separators() {
        let tail = String::from("N-12.345 AB");
        assert_eq!(normalize_tail(&tail), "N12345AB");
    }

    #[test]
    fn test_normalize_tail_empty_string() {
        let tail = String::from("");
        assert_eq!(normalize_tail(&tail), "");
    }

    #[test]
    fn test_normalize_tail_only_separators() {
        let tail = String::from(".- -.");
        assert_eq!(normalize_tail(&tail), "");
    }
}
