pub(crate) fn is_pow_of_two(val: u32) -> bool {
    if val == 0 {
        return false;
    }
    (val & (val - 1)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_powers_of_two() {
        assert_eq!(is_pow_of_two(0), false);
        assert_eq!(is_pow_of_two(1), true);
        assert_eq!(is_pow_of_two(2), true);
        assert_eq!(is_pow_of_two(3), false);
        assert_eq!(is_pow_of_two(4), true);
        assert_eq!(is_pow_of_two(8), true);
        assert_eq!(is_pow_of_two(15), false);
        assert_eq!(is_pow_of_two(16), true);
        assert_eq!(is_pow_of_two(20), false);
    }
}
