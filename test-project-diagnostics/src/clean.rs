// This file is intentionally clean with no errors, warnings, or hints.

/// A simple function that adds two numbers.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// A simple struct with basic functionality.
#[derive(Debug, Clone, PartialEq)]
pub struct Calculator {
    value: i32,
}

impl Calculator {
    /// Creates a new Calculator with the given initial value.
    pub fn new(value: i32) -> Self {
        Calculator { value }
    }

    /// Gets the current value.
    pub fn value(&self) -> i32 {
        self.value
    }

    /// Adds to the current value.
    pub fn add(&mut self, amount: i32) {
        self.value += amount;
    }

    /// Multiplies the current value.
    pub fn multiply(&mut self, factor: i32) {
        self.value *= factor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
    }

    #[test]
    fn test_calculator() {
        let mut calc = Calculator::new(10);
        calc.add(5);
        assert_eq!(calc.value(), 15);
        calc.multiply(2);
        assert_eq!(calc.value(), 30);
    }
}